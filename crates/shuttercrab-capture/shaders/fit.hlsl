// Shuttercrab: a recorded window's picture, fitted into the recording's frame.
//
// A window recording keeps the size the window had when it started. A window
// resized since is scaled to fit inside that frame, keeping its shape, with
// black bars where it does not fill it.
//
// Each output pixel averages up to 4x4 bilinear samples spread over the part
// of the picture it covers, so a window scaled down keeps its text readable
// rather than shimmering. Values stay linear scRGB, as captured.

cbuffer Fit : register(b0)
{
    uint2 outputSize;   // the frame, pixels
    float2 offset;      // where the picture starts in the frame, pixels
    float2 fitted;      // the picture's size in the frame, pixels
    float2 contentSize; // the picture's size in the source, pixels
    float2 sourceSize;  // the source texture's size, pixels
    uint samples;       // samples per side, 1 to 4
    uint unused;
};

Texture2D<float4> source : register(t0);
SamplerState linearClamp : register(s0);
RWTexture2D<float4> destination : register(u0);

[numthreads(8, 8, 1)]
void fit(uint3 id : SV_DispatchThreadID)
{
    if (id.x >= outputSize.x || id.y >= outputSize.y)
        return;
    // This pixel's centre, from the picture's top left, in frame pixels.
    float2 at = float2(id.xy) + 0.5 - offset;
    if (any(at < 0.0) || any(at >= fitted))
    {
        destination[id.xy] = float4(0.0, 0.0, 0.0, 1.0);
        return;
    }
    // Source pixels per frame pixel.
    float2 scale = contentSize / fitted;
    float4 sum = 0.0;
    for (uint y = 0; y < samples; y++)
    {
        for (uint x = 0; x < samples; x++)
        {
            float2 inside = (float2(x, y) + 0.5) / samples - 0.5;
            float2 position = clamp((at + inside) * scale, 0.5, contentSize - 0.5);
            sum += source.SampleLevel(linearClamp, position / sourceSize, 0);
        }
    }
    destination[id.xy] = float4(sum.rgb / (samples * samples), 1.0);
}
