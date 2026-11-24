// Bevy's StandardMaterial forward fragment with one step: the piece's colour from its `MeshTag` slot.
import bevy_pbr::{
    render::{
        pbr_types,
        pbr_functions::alpha_discard,
        pbr_fragment::pbr_input_from_standard_material,
        mesh_functions::get_tag,
    },
};

@if(PREPASS_PIPELINE)
import bevy_pbr::{
    prepass::io::{VertexOutput, FragmentOutput},
    deferred::functions::deferred_output,
};
@else
import bevy_pbr::render::{
    forward_io::{VertexOutput, FragmentOutput},
    pbr_functions::{apply_pbr_lighting, main_pass_post_lighting_processing},
    pbr_types::STANDARD_MATERIAL_FLAGS_UNLIT_BIT,
};

@if(VISIBILITY_RANGE_DITHER)
import bevy_pbr::render::pbr_functions::visibility_range_dither;

@if(MATERIAL_OIT_ENABLED)
import bevy_core_pipeline::oit::draw::oit_draw;

@group(3) @binding(100) var<storage, read> tints: array<vec4<f32>>;

// Only solid pieces (puke chunks) carry vertex colours; their facets are shaded by a light from above, down to this
// share in shadow, so a lump reads as a lump.
const SOLID_KEY: vec3<f32> = vec3(0.3, 0.9, 0.3);
const SOLID_SHADOW: f32 = 0.4;

@fragment
fn fragment(vertex_output: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var in = vertex_output;

@if(VISIBILITY_RANGE_DITHER)
    visibility_range_dither(in.position, in.visibility_range_dither);

    var pbr_input = pbr_input_from_standard_material(in, is_front);
    pbr_input.material.base_color *= tints[get_tag(in.instance_index)];

@if(VERTEX_COLORS && !PREPASS_PIPELINE) {
    let lit = SOLID_SHADOW + (1.0 - SOLID_SHADOW) * saturate(dot(normalize(in.world_normal), SOLID_KEY));
    pbr_input.material.base_color = vec4(pbr_input.material.base_color.rgb * lit, pbr_input.material.base_color.a);
}

    pbr_input.material.base_color = alpha_discard(
        pbr_input.material.flags,
        pbr_input.material.alpha_cutoff,
        pbr_input.material.base_color
    );

    // No clustered decals: the liquid layer's and the runs' are data, not colour, and would paint a piece lying in them.

@if(PREPASS_PIPELINE)
    let out = deferred_output(in, pbr_input);
@if(!PREPASS_PIPELINE)
    var out: FragmentOutput;
@if(!PREPASS_PIPELINE)
    if (pbr_input.material.flags & STANDARD_MATERIAL_FLAGS_UNLIT_BIT) == 0u {
        out.color = apply_pbr_lighting(pbr_input);
    } else {
        out.color = pbr_input.material.base_color;
    }

@if(!PREPASS_PIPELINE)
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);

@if(MATERIAL_OIT_ENABLED) {
    let alpha_mode = pbr_input.material.flags & pbr_types::STANDARD_MATERIAL_FLAGS_ALPHA_MODE_RESERVED_BITS;
    if alpha_mode == pbr_types::STANDARD_MATERIAL_FLAGS_ALPHA_MODE_BLEND {
        oit_draw(in.position, vec4(out.color.rgb * out.color.a, out.color.a));
        discard;
    }
    if alpha_mode == pbr_types::STANDARD_MATERIAL_FLAGS_ALPHA_MODE_PREMULTIPLIED
        || alpha_mode == pbr_types::STANDARD_MATERIAL_FLAGS_ALPHA_MODE_ADD {
        oit_draw(in.position, out.color);
        discard;
    }
}

    return out;
}
