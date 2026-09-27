// Framecut HDR/WCG -> SDR transform. Three compute passes over one captured frame,
// with a small CPU step between the second and third:
//
//   classify    source scRGB              -> 1 where a pixel is an SDR code value
//   tile_stats  source + classes          -> per 16x16 tile: peak, non-SDR and
//                                            extended counts, non-SDR bounding box
//   (CPU)       tile stats                -> HDR regions (color.rs, find_regions)
//   convert     source + regions          -> packed RGBA8 sRGB
//
// Input is linear scRGB (BT.709 primaries, 1.0 = 80 nits in HDR mode), never PQ
// and never sRGB-encoded. src/color.rs is the CPU reference for every function
// here, and docs/COLOR_PIPELINE.md derives the constants.

cbuffer Params : register(b0)
{
    float whiteScale;    // S: scRGB value of SDR white
    uint width;
    uint height;
    uint tilesX;
    uint tilesY;
    uint highlightMode;  // 0 = tone map HDR regions, 1 = clip along the RGB ray
    uint2 unused;
};

static const uint MAX_REGIONS = 32;

cbuffer Regions : register(b1)
{
    uint regionCount;
    uint3 regionsUnused;
    uint4 regionRects[MAX_REGIONS];      // x0, y0, x1, y1, inclusive
    float4 regionPeaks[MAX_REGIONS / 4]; // peak of region i in [i / 4][i % 4]
};

struct TileStat
{
    float peak;
    uint nonSdr;
    uint extended;
    uint minX;
    uint minY;
    uint maxX;
    uint maxY;
    uint unused;
};

Texture2D<float4> source : register(t0);
Texture2D<float> classes : register(t1);
RWTexture2D<float> classesOut : register(u0);
RWTexture2D<uint> destination : register(u1);
RWStructuredBuffer<TileStat> tileStats : register(u2);

static const uint TILE = 16;
static const float EXTENDED_EPSILON = 1.0 / 256.0;
static const float CODE_TOLERANCE = 0.25;
static const float NEGLIGIBLE = 0.005;
static const float CROSS_TALK = 0.001;
static const float TONE_KNEE = 0.45;
static const float FP16_MAX = 65504.0;
static const float3 LUMA = float3(0.2126, 0.7152, 0.0722);

// Divide by SDR white; desaturate out-of-gamut colors toward their luminance.
float3 normalizeScRgb(float3 c)
{
    c = isnan(c) ? 0.0 : clamp(c, -FP16_MAX, FP16_MAX);
    float3 n = c / whiteScale;
    float lo = min(n.r, min(n.g, n.b));
    if (lo >= 0.0)
        return n;
    float y = dot(n, LUMA);
    if (y <= 0.0)
        return 0.0;
    float t = y / (y - lo);
    return max(y + t * (n - y), 0.0);
}

float peak(float3 n)
{
    return max(n.r, max(n.g, n.b));
}

float srgbEncode(float l)
{
    l = saturate(l);
    return l <= 0.0031308 ? 12.92 * l : 1.055 * pow(l, 1.0 / 2.4) - 0.055;
}

float srgbDecode(float e)
{
    return e <= 0.04045 ? e / 12.92 : pow((e + 0.055) / 1.055, 2.4);
}

uint quantize(float e)
{
    return (uint)floor(saturate(e) * 255.0 + 0.5);
}

// A channel is an SDR code value when it is negligible, or within SDR white and
// S x decode(code) for some 8-bit code to within CODE_TOLERANCE codes, or to
// within CROSS_TALK x the pixel's peak in linear light.
bool onCodeGrid(float3 c)
{
    float3 n = c / whiteScale;
    if (any(isnan(n)))
        return false;
    bool3 negligible = abs(n) < NEGLIGIBLE;
    if (any(!negligible && (n < 0.0 || n > 1.0 + EXTENDED_EPSILON)))
        return false;
    float top = peak(n);
    float3 code = float3(srgbEncode(n.r), srgbEncode(n.g), srgbEncode(n.b)) * 255.0;
    float3 nearest = round(code);
    float3 decoded = float3(srgbDecode(nearest.r / 255.0), srgbDecode(nearest.g / 255.0), srgbDecode(nearest.b / 255.0));
    return all(negligible || abs(code - nearest) <= CODE_TOLERANCE || abs(n - decoded) <= CROSS_TALK * top);
}

[numthreads(8, 8, 1)]
void classify(uint3 id : SV_DispatchThreadID)
{
    if (id.x >= width || id.y >= height)
        return;
    classesOut[id.xy] = onCodeGrid(source.Load(int3(id.xy, 0)).rgb) ? 1.0 : 0.0;
}

// SDR content: the pixel and its eight neighbours are all SDR code values.
bool sdrContent(uint2 p)
{
    int2 last = int2(width - 1, height - 1);
    for (int dy = -1; dy <= 1; dy++)
        for (int dx = -1; dx <= 1; dx++)
            if (classes.Load(int3(clamp(int2(p) + int2(dx, dy), 0, last), 0)) < 0.5)
                return false;
    return true;
}

groupshared float gsPeak[TILE * TILE];
groupshared uint gsNonSdr;
groupshared uint gsExtended;
groupshared uint gsMinX;
groupshared uint gsMinY;
groupshared uint gsMaxX;
groupshared uint gsMaxY;

[numthreads(TILE, TILE, 1)]
void tile_stats(uint3 group : SV_GroupID, uint3 thread : SV_GroupThreadID, uint index : SV_GroupIndex)
{
    if (index == 0)
    {
        gsNonSdr = 0;
        gsExtended = 0;
        gsMinX = 0xFFFFFFFF;
        gsMinY = 0xFFFFFFFF;
        gsMaxX = 0;
        gsMaxY = 0;
    }
    GroupMemoryBarrierWithGroupSync();
    uint2 p = group.xy * TILE + thread.xy;
    float m = 0.0;
    if (p.x < width && p.y < height)
    {
        m = peak(normalizeScRgb(source.Load(int3(p, 0)).rgb));
        uint ignored;
        if (m > 1.0 + EXTENDED_EPSILON)
            InterlockedAdd(gsExtended, 1, ignored);
        if (!sdrContent(p))
        {
            InterlockedAdd(gsNonSdr, 1, ignored);
            InterlockedMin(gsMinX, p.x, ignored);
            InterlockedMin(gsMinY, p.y, ignored);
            InterlockedMax(gsMaxX, p.x, ignored);
            InterlockedMax(gsMaxY, p.y, ignored);
        }
    }
    gsPeak[index] = m;
    GroupMemoryBarrierWithGroupSync();
    [unroll]
    for (uint stride = TILE * TILE / 2; stride > 0; stride >>= 1)
    {
        if (index < stride)
            gsPeak[index] = max(gsPeak[index], gsPeak[index + stride]);
        GroupMemoryBarrierWithGroupSync();
    }
    if (index == 0)
    {
        TileStat s;
        s.peak = gsPeak[0];
        s.nonSdr = gsNonSdr;
        s.extended = gsExtended;
        s.minX = gsMinX;
        s.minY = gsMinY;
        s.maxX = gsMaxX;
        s.maxY = gsMaxY;
        s.unused = 0;
        tileStats[group.y * tilesX + group.x] = s;
    }
}

// HDR content with peak p: dim by 1/sqrt(p), linear to TONE_KNEE, then an
// extended-Reinhard shoulder reaching 1 at the peak.
float hdrCurve(float m, float p)
{
    p = max(p, 1.0);
    float g = 1.0 / sqrt(p);
    float top = max(sqrt(p), TONE_KNEE + 1e-3);
    float x = g * m;
    if (x <= TONE_KNEE)
        return x;
    float a = (min(x, top) - TONE_KNEE) / (1.0 - TONE_KNEE);
    float bigA = (top - TONE_KNEE) / (1.0 - TONE_KNEE);
    return TONE_KNEE + (1.0 - TONE_KNEE) * a * (1.0 + a / (bigA * bigA)) / (1.0 + a);
}

[numthreads(8, 8, 1)]
void convert(uint3 id : SV_DispatchThreadID)
{
    if (id.x >= width || id.y >= height)
        return;
    float3 n = normalizeScRgb(source.Load(int3(id.xy, 0)).rgb);
    if (highlightMode == 0)
    {
        for (uint i = 0; i < regionCount; i++)
        {
            uint4 r = regionRects[i];
            if (id.x >= r.x && id.y >= r.y && id.x <= r.z && id.y <= r.w)
            {
                float m = peak(n);
                if (m > 0.0)
                    n *= hdrCurve(m, regionPeaks[i / 4][i % 4]) / m;
                break;
            }
        }
    }
    n /= max(1.0, peak(n));
    uint r = quantize(srgbEncode(n.r));
    uint g = quantize(srgbEncode(n.g));
    uint b = quantize(srgbEncode(n.b));
    destination[id.xy] = r | (g << 8) | (b << 16) | (255u << 24);
}
