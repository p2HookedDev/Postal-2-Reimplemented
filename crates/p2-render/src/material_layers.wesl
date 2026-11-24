// The layers of a material whose data is `ShaderLayers` itself; the actor shader wraps its own and declares it.

import bevy_pbr::render::forward_io::VertexOutput;
import package::p2_render::layer_bindings::ShaderLayers;
@if(BINDLESS)
import package::p2_render::layer_bindings::indices;

@if(BINDLESS)
@group(3) @binding(107) var<storage> layers_array: array<ShaderLayers>;
@else
@group(3) @binding(100) var<uniform> layers_data: ShaderLayers;

fn material_layers(in: VertexOutput) -> ShaderLayers {
@if(BINDLESS)
    return layers_array[indices(in).data];
@else
    return layers_data;
}
