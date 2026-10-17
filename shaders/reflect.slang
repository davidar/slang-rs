// Reflection fixtures: resources in two descriptor sets, a struct with a
// known layout, string literals hashed with getStringHash, and an entry
// point with no [shader] attribute, for findAndCheckEntryPoint.

struct Params {
	float4 color;
	uint count;
	float2 offset;
	uint flags;
};

[[vk::binding(0, 0)]] ConstantBuffer<Params> params;
[[vk::binding(1, 0)]] RWStructuredBuffer<uint> output;
[[vk::binding(0, 1)]] Texture2DArray<float4> layers;
[[vk::binding(1, 1)]] SamplerState linear_sampler;

[numthreads(1, 1, 1)]
void probe(uint3 id : SV_DispatchThreadID) {
	float4 texel = layers.SampleLevel(linear_sampler, float3(params.offset, 0.0), 0.0);
	output[id.x] = getStringHash("hello") ^ getStringHash("world") ^ params.count ^ params.flags
		^ uint(texel.x * params.color.y);
}
