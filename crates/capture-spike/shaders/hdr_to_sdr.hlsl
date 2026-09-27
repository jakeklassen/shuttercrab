// Framecut HDR/WCG -> SDR transform. Four compute passes over one captured frame:
//
//   tile_peak   source scRGB          -> peak of each 16x16 tile, normalized
//   frame_peak  tile peaks            -> peak of the whole frame (1x1)
//   proximity   tile peaks            -> 1 near extended tiles, else 0
//   convert     source + both maps    -> packed RGBA8 sRGB
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
    uint highlightMode;  // 0 = shoulder, 1 = clip along the RGB ray
    uint2 unused;
};

Texture2D<float4> source : register(t0);
Texture2D<float> tileMap : register(t1);    // peaks, or proximity for convert
Texture2D<float> framePeakIn : register(t2);
RWTexture2D<float> tileOut : register(u0);
RWTexture2D<uint> destination : register(u1);

static const uint TILE = 16;
static const int DILATE_TILES = 1;
static const float EXTENDED_EPSILON = 1.0 / 256.0;
static const float SHOULDER_BETA = 0.25;
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

// Identity up to the knee, then an extended-Reinhard shoulder landing on 1 at h.
float shoulder(float x, float h)
{
    float k = 1.0 - SHOULDER_BETA * (1.0 - 1.0 / h);
    if (x <= k)
        return x;
    x = min(x, h);
    float a = (x - k) / (1.0 - k);
    float bigA = (h - k) / (1.0 - k);
    return k + (1.0 - k) * (a * (1.0 + a / (bigA * bigA)) / (1.0 + a));
}

float srgbEncode(float l)
{
    l = saturate(l);
    return l <= 0.0031308 ? 12.92 * l : 1.055 * pow(l, 1.0 / 2.4) - 0.055;
}

uint quantize(float e)
{
    return (uint)floor(saturate(e) * 255.0 + 0.5);
}

groupshared float scratch[1024];

[numthreads(TILE, TILE, 1)]
void tile_peak(uint3 group : SV_GroupID, uint3 thread : SV_GroupThreadID, uint index : SV_GroupIndex)
{
    uint2 p = group.xy * TILE + thread.xy;
    float m = 0.0;
    if (p.x < width && p.y < height)
        m = peak(normalizeScRgb(source.Load(int3(p, 0)).rgb));
    scratch[index] = m;
    GroupMemoryBarrierWithGroupSync();
    [unroll]
    for (uint stride = TILE * TILE / 2; stride > 0; stride >>= 1)
    {
        if (index < stride)
            scratch[index] = max(scratch[index], scratch[index + stride]);
        GroupMemoryBarrierWithGroupSync();
    }
    if (index == 0)
        tileOut[group.xy] = scratch[0];
}

[numthreads(1024, 1, 1)]
void frame_peak(uint index : SV_GroupIndex)
{
    float m = 0.0;
    for (uint i = index; i < tilesX * tilesY; i += 1024)
        m = max(m, tileMap.Load(int3(i % tilesX, i / tilesX, 0)));
    scratch[index] = m;
    GroupMemoryBarrierWithGroupSync();
    [unroll]
    for (uint stride = 512; stride > 0; stride >>= 1)
    {
        if (index < stride)
            scratch[index] = max(scratch[index], scratch[index + stride]);
        GroupMemoryBarrierWithGroupSync();
    }
    if (index == 0)
        tileOut[uint2(0, 0)] = scratch[0];
}

[numthreads(8, 8, 1)]
void proximity(uint3 id : SV_DispatchThreadID)
{
    if (id.x >= tilesX || id.y >= tilesY)
        return;
    int2 last = int2(tilesX - 1, tilesY - 1);
    float near = 0.0;
    for (int dy = -DILATE_TILES; dy <= DILATE_TILES; dy++)
        for (int dx = -DILATE_TILES; dx <= DILATE_TILES; dx++)
            if (tileMap.Load(int3(clamp(int2(id.xy) + int2(dx, dy), 0, last), 0)) > 1.0 + EXTENDED_EPSILON)
                near = 1.0;
    tileOut[id.xy] = near;
}

// Bilinear interpolation of the proximity map between tile centres.
float weightAt(uint2 p)
{
    float2 last = float2(tilesX - 1, tilesY - 1);
    float2 u = clamp((float2(p) + 0.5) / TILE - 0.5, 0.0, last);
    uint2 i0 = (uint2)floor(u);
    uint2 i1 = min(i0 + 1, uint2(last));
    float2 f = u - float2(i0);
    float v00 = tileMap.Load(int3(i0.x, i0.y, 0));
    float v10 = tileMap.Load(int3(i1.x, i0.y, 0));
    float v01 = tileMap.Load(int3(i0.x, i1.y, 0));
    float v11 = tileMap.Load(int3(i1.x, i1.y, 0));
    float top = v00 + (v10 - v00) * f.x;
    float bottom = v01 + (v11 - v01) * f.x;
    return top + (bottom - top) * f.y;
}

[numthreads(8, 8, 1)]
void convert(uint3 id : SV_DispatchThreadID)
{
    if (id.x >= width || id.y >= height)
        return;
    float3 n = normalizeScRgb(source.Load(int3(id.xy, 0)).rgb);
    if (highlightMode == 0)
    {
        float framePeak = framePeakIn.Load(int3(0, 0, 0));
        float w = weightAt(id.xy);
        float m = peak(n);
        if (framePeak > 1.0 + EXTENDED_EPSILON && w > 0.0 && m > 0.0)
            n *= (m + w * (shoulder(m, framePeak) - m)) / m;
    }
    n /= max(1.0, peak(n));
    uint r = quantize(srgbEncode(n.r));
    uint g = quantize(srgbEncode(n.g));
    uint b = quantize(srgbEncode(n.b));
    destination[id.xy] = r | (g << 8) | (b << 16) | (255u << 24);
}
