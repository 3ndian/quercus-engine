// Quercus Video Editor - Non-Linear Video Editor
// Copyright (C) 2026 Quercus Team
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

//! VHS REVRB: a 2000s home-video look (cheap DV camera plus VHS tape),
//! ported from the Revrb Android app's `VideoLookEngine`.
//!
//! Sliders (all 0..1, shown as percentages):
//!
//! * **DV halo** - unsharp-mask rims around edges, like a cheap DV camera
//!   with no limiter. **Halo size** sets how far the rim reaches.
//! * **DV crush** - hard S-curve with crushed blacks and slightly hot
//!   highlights.
//! * **VHS bleed** - colour smears sideways and arrives a little late
//!   (colour is a lower-resolution signal than brightness). The I and Q
//!   colour signals blur by different amounts, as in NTSC.
//! * **VHS soft** - horizontal softness on the brightness signal.
//! * **VHS noise** - per-line jitter, tape grain, colour flicker and the
//!   odd dropout streak. The pattern changes every frame.
//!
//! The Android app runs on 8-bit gamma pixels one row at a time. Here
//! everything is one fragment pass, so: the picture is encoded to a
//! gamma-2.2 space first and decoded after (the sliders then behave like
//! the app's); the row box blurs become fixed-tap stratified blurs; the
//! halo blur is a spiral disc blur; and the halo reads the source picture
//! (the app ran it before the VHS pass). Not ported yet: the head-switching
//! band, TV I/Q/fringe and the rainbow strip.
//!
//! With every slider at 0 the output is the input.

use crate::factory::NodeMeta;
use crate::jobs::{Job, ShaderJobPayload};
use crate::node::{Category, NodeBehavior, NodeCore};

/// Texture input id (the effect input).
pub const TEXTURE_INPUT: &str = "tex_in";
/// DV halo amount, float 0..1, default 0.3.
pub const DV_HALO_INPUT: &str = "dv_halo_in";
/// Halo size, float 0..1, default 0.2 (the Revrb app's rest position).
pub const DV_SIZE_INPUT: &str = "dv_size_in";
/// DV crush amount, float 0..1, default 0.3.
pub const DV_CRUSH_INPUT: &str = "dv_crush_in";
/// VHS colour bleed, float 0..1, default 0.5.
pub const VHS_BLEED_INPUT: &str = "vhs_bleed_in";
/// VHS horizontal softness, float 0..1, default 0.3.
pub const VHS_SOFT_INPUT: &str = "vhs_soft_in";
/// VHS tape noise, float 0..1, default 0.2.
pub const VHS_NOISE_INPUT: &str = "vhs_noise_in";

/// Every float slider: (input id, display name, default).
const SLIDERS: [(&str, &str, f64); 6] = [
	(DV_HALO_INPUT, "DV halo", 0.3),
	(DV_SIZE_INPUT, "Halo size", 0.2),
	(DV_CRUSH_INPUT, "DV crush", 0.3),
	(VHS_BLEED_INPUT, "VHS bleed", 0.5),
	(VHS_SOFT_INPUT, "VHS soft", 0.3),
	(VHS_NOISE_INPUT, "VHS noise", 0.2),
];

/// VHS REVRB node.
pub struct VhsRevrbNode;

/// Fragment shader. No `switch`, no early returns, and no texture fetch
/// under a per-pixel condition (naga's uniformity rules).
const SHADER_FRAG: &str = r#"uniform sampler2D tex_in;
uniform vec2 resolution_in;
uniform float time_in;
uniform float dv_halo_in;
uniform float dv_size_in;
uniform float dv_crush_in;
uniform float vhs_bleed_in;
uniform float vhs_soft_in;
uniform float vhs_noise_in;

in vec2 ove_texcoord;
out vec4 frag_color;

#define CHROMA_TAPS 24
#define SOFT_TAPS 9
#define HALO_TAPS 24
#define GAMMA 2.2

float vr_hash(float n) {
  return fract(sin(n * 12.9898) * 43758.5453);
}

vec3 vr_enc(vec3 c) {
  return pow(max(c, vec3(0.0)), vec3(1.0 / GAMMA));
}

vec3 vr_dec(vec3 c) {
  return pow(max(c, vec3(0.0)), vec3(GAMMA));
}

vec3 vr_fetch(vec2 uv) {
  vec2 half_texel = vec2(0.5) / resolution_in;
  return vr_enc(texture(tex_in, clamp(uv, half_texel, vec2(1.0) - half_texel)).rgb);
}

float vr_luma(vec3 c) {
  return dot(c, vec3(0.299, 0.587, 0.114));
}

vec2 vr_iq(vec3 c) {
  return vec2(
    0.5959 * c.r - 0.2746 * c.g - 0.3213 * c.b,
    0.2115 * c.r - 0.5227 * c.g + 0.3112 * c.b);
}

vec3 vr_from_yiq(float y, float i, float q) {
  return vec3(
    y + 0.956 * i + 0.621 * q,
    y - 0.272 * i - 0.647 * q,
    y - 1.106 * i + 1.703 * q);
}

// S-curve with crushed blacks and a hot top end (the app's `curve`).
vec3 vr_curve(vec3 c, float k) {
  vec3 t = clamp(c, vec3(0.0), vec3(1.0));
  vec3 s = t * t * (vec3(3.0) - 2.0 * t);
  vec3 m = t + (s - t) * (0.9 * k);
  return m * (1.0 + 0.08 * k) - vec3(0.03 * k);
}

void main() {
  vec2 res = resolution_in;
  float w = res.x;
  float h = res.y;
  float longside = max(w, h);
  float px_x = ove_texcoord.x * w;
  float px_y = floor(ove_texcoord.y * h);

  float halo = clamp(dv_halo_in, 0.0, 1.0);
  float crush = clamp(dv_crush_in, 0.0, 1.0);
  float bleed = clamp(vhs_bleed_in, 0.0, 1.0);
  float soft = clamp(vhs_soft_in, 0.0, 1.0);
  float noise = clamp(vhs_noise_in, 0.0, 1.0);

  // The tape pattern changes every frame (24 steps a second).
  float seed = floor(time_in * 24.0) * 7.31;

  // ---- VHS: per-line jitter, then the three signals ----
  float shift = (vr_hash(floor(px_y / 3.0) + 17.0 + seed) - 0.5) * noise * 0.004 * w;
  vec2 uv = ove_texcoord - vec2(shift / w, 0.0);
  vec3 center = vr_fetch(uv);
  vec4 source = texture(tex_in, ove_texcoord);

  // Luma softness: horizontal box.
  float soft_rad = soft > 0.0 ? max(1.0, soft * 0.004 * w) : 0.0;
  float luma_acc = 0.0;
  for (int k = 0; k < SOFT_TAPS; ++k) {
    float f = (float(k) + 0.5) / float(SOFT_TAPS) * 2.0 - 1.0;
    luma_acc += vr_luma(vr_fetch(uv + vec2(f * soft_rad / w, 0.0)));
  }
  float luma = luma_acc / float(SOFT_TAPS);

  // Colour bleed: I and Q boxes of different widths, sampled late.
  float bx = bleed * bleed;
  float rad_i = bleed > 0.0 ? max(1.0, (bleed * 0.075 + bx * 0.05) * w) : 0.0;
  float rad_q = bleed > 0.0 ? max(1.0, (bleed * 0.075 + bx * 0.11) * w) : 0.0;
  float delay = (bleed * 0.024 + 2.0 * bx * 0.006) * w;
  float i_acc = 0.0;
  float q_acc = 0.0;
  for (int k = 0; k < CHROMA_TAPS; ++k) {
    float f = (float(k) + 0.5) / float(CHROMA_TAPS) * 2.0 - 1.0;
    i_acc += vr_iq(vr_fetch(uv - vec2((delay - f * rad_i) / w, 0.0))).x;
    q_acc += vr_iq(vr_fetch(uv - vec2((delay - f * rad_q) / w, 0.0))).y;
  }
  float chroma_i = i_acc / float(CHROMA_TAPS);
  float chroma_q = q_acc / float(CHROMA_TAPS);

  // Tape noise: grain smeared along the line, a colour flicker per line,
  // and short bright dropout dashes.
  float amp = noise * 30.0 / 255.0;
  float grain_a = vr_hash(px_x * 374.761 + px_y * 668.265 + seed * 7.0) - 0.5;
  float grain_b = vr_hash((px_x - 1.0) * 374.761 + px_y * 668.265 + seed * 7.0) - 0.5;
  luma += (0.45 * grain_a + 0.55 * grain_b) * amp * 2.4;
  float flicker = (vr_hash(px_y * 53.0 + seed) - 0.5) * noise * 14.0 / 255.0;
  chroma_i += flicker;
  chroma_q -= flicker * 0.7;
  float line_hit = step(1.0 - 0.0035 * noise, vr_hash(px_y * 13.0 + seed * 3.0 + 1.0));
  float drop_x0 = vr_hash(px_y + 99.0 + seed) * w * 0.85;
  float drop_len = max(3.0, w * (0.02 + 0.10 * vr_hash(px_y + 41.0 + seed)));
  float in_drop = line_hit * step(drop_x0, px_x) * step(px_x, drop_x0 + drop_len);
  luma += in_drop * 95.0 / 255.0 * noise;
  chroma_i *= mix(1.0, 0.3, in_drop);
  chroma_q *= mix(1.0, 0.3, in_drop);

  // Replace the centre pixel's Y/I/Q with the processed ones. Adding the
  // difference (not rebuilding the colour) keeps the picture exactly
  // unchanged when every slider is 0.
  vec2 center_iq = vr_iq(center);
  vec3 rgb = center + vr_from_yiq(
    luma - vr_luma(center), chroma_i - center_iq.x, chroma_q - center_iq.y);

  // ---- DV halo: unsharp mask against a disc blur of the source ----
  float n = dv_size_in;
  float d = n >= 0.2 ? (n - 0.2) / 0.8 : (n - 0.2) / 0.2;
  float s = d >= 0.0 ? -0.6 + 1.6 * d : -0.6 + 0.4 * d;
  float halo_rad = max(1.0, longside * (0.011 + 0.008 * s));
  float blur_acc = 0.0;
  for (int k = 0; k < HALO_TAPS; ++k) {
    float r = halo_rad * sqrt((float(k) + 0.5) / float(HALO_TAPS));
    float a = float(k) * 2.3999632;
    blur_acc += vr_luma(vr_fetch(ove_texcoord + vec2(cos(a), sin(a)) * r / res));
  }
  float luma_blur = blur_acc / float(HALO_TAPS);
  float rim = clamp(halo * 2.2 * (vr_luma(vr_fetch(ove_texcoord)) - luma_blur), -90.0 / 255.0, 90.0 / 255.0);
  rgb += vec3(rim);

  // ---- DV crush ----
  if (crush > 0.0) {
    rgb = vr_curve(rgb, crush);
  }

  frag_color = vec4(vr_dec(rgb), source.a);
}
"#;

impl VhsRevrbNode {
	/// Fragment shader for any request (single variant).
	fn shader_frag() -> &'static str {
		SHADER_FRAG
	}
}

impl NodeBehavior for VhsRevrbNode {
	/// Human-readable name.
	fn name(&self) -> &str {
		"VHS REVRB"
	}

	/// Stable type id.
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.vhsrevrb"
	}

	/// Categories.
	fn categories(&self) -> &[Category] {
		&[Category::Filter]
	}

	/// Description.
	fn description(&self) -> &str {
		"2000s home-video look: DV halo and crush, VHS colour bleed, softness and tape noise."
	}

	/// Input names.
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		if id == TEXTURE_INPUT {
			return "Input";
		}
		for (slider_id, name, _) in SLIDERS {
			if id == slider_id {
				return name;
			}
		}
		id
	}

	/// Pushes the shader job. Each slider is read from the row (a keyframed
	/// or connected value) or else from the node's own value at `time`;
	/// `time_in` (seconds) drives the per-frame tape pattern.
	fn value(
		&self,
		core: &NodeCore,
		inputs: &crate::value::NodeValueRow,
		time: quercus_core::Rational,
		table: &mut crate::value::NodeValueTable,
	) {
		use crate::value::{NodeValue, ValueType};

		match inputs.get(TEXTURE_INPUT) {
			Some(NodeValue::Texture(_)) => {}
			_ => return,
		}

		let mut params = inputs.clone();
		for (id, _, _) in SLIDERS {
			let v = match inputs.get(id) {
				Some(v) => v.to_double(),
				None => core.value_at_time(id, -1, time).to_double(),
			};
			params.insert(id.to_string(), NodeValue::Float(v));
		}
		params.insert("time_in".to_string(), NodeValue::Float(time.to_f64()));

		table.push(
			ValueType::Texture,
			NodeValue::Texture(crate::handle::make_owned(Job::ShaderJob(ShaderJobPayload {
				node_id: crate::id::NodeId::INVALID,
				time,
				iterations: 1,
				type_id: self.type_id().to_string(),
				shader_id: String::new(),
				effect_input: core.effect_input.clone(),
				params,
				iterative_input: String::new(),
			}))),
			None,
		);
	}

	/// Shader code request: always the one fragment source.
	fn shader_code(&self, _request: &str) -> Option<String> {
		Some(Self::shader_frag().to_string())
	}

	/// Deep copy.
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(VhsRevrbNode))
	}
}

/// Constructor: the texture input plus one 0..1 percentage slider per
/// entry of [`SLIDERS`]; video effect on `tex_in`.
pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
	let mut core = NodeCore::new();

	let mut tex = crate::input::Input::new(
		TEXTURE_INPUT,
		crate::value::ValueType::Texture,
		crate::value::NodeValue::None,
	);
	tex.flags |= crate::input::flags::NOT_KEYFRAMABLE;
	core.add_input(tex);

	for (id, _, default) in SLIDERS {
		let mut slider = crate::input::Input::new(
			id,
			crate::value::ValueType::Float,
			crate::value::NodeValue::Float(default),
		);
		slider.properties = vec![
			(
				"view".to_string(),
				crate::value::NodeValue::Text("percentage".into()),
			),
			("min".to_string(), crate::value::NodeValue::Float(0.0)),
			("max".to_string(), crate::value::NodeValue::Float(1.0)),
		];
		core.add_input(slider);
	}

	core.flags |= crate::node::flags::VIDEO_EFFECT;
	core.effect_input = TEXTURE_INPUT.to_string();

	(core, Box::new(VhsRevrbNode))
}

/// Register this node type.
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.vhsrevrb",
		name: "VHS REVRB",
		categories: &[Category::Filter],
		create,
	});
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::value::{NodeValue, NodeValueTable, ValueType};
	use quercus_core::Rational;

	fn tex() -> NodeValue {
		NodeValue::Texture(crate::handle::CHandle::null())
	}

	fn run(core: &NodeCore, inputs: &crate::value::NodeValueRow) -> ShaderJobPayload {
		let behavior = VhsRevrbNode;
		let mut table = NodeValueTable::default();
		behavior.value(core, inputs, Rational::new(0, 1), &mut table);
		let Some(NodeValue::Texture(h)) = table.get(ValueType::Texture) else {
			panic!("shader job expected");
		};
		unsafe { crate::jobs::shader_job(h) }
			.expect("shader job payload boxed")
			.clone()
	}

	#[test]
	fn create_wires_inputs_and_flags() {
		let (core, behavior) = create();
		assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.vhsrevrb");
		assert_eq!(behavior.name(), "VHS REVRB");
		assert_eq!(core.effect_input, TEXTURE_INPUT);
		assert_ne!(core.flags & crate::node::flags::VIDEO_EFFECT, 0);
		for (id, name, default) in SLIDERS {
			assert_eq!(
				core.get_input(id).expect("slider input").default,
				NodeValue::Float(default)
			);
			assert_eq!(VhsRevrbNode.input_name(id), name);
		}
	}

	#[test]
	fn value_no_texture_pushes_nothing() {
		let (core, _) = create();
		let mut table = NodeValueTable::default();
		VhsRevrbNode.value(
			&core,
			&crate::value::NodeValueRow::default(),
			Rational::new(0, 1),
			&mut table,
		);
		assert!(table.is_empty());
	}

	#[test]
	fn job_carries_every_slider_and_time() {
		let (mut core, _) = create();
		core.set_standard_value(VHS_BLEED_INPUT, -1, NodeValue::Float(0.9));
		let inputs = crate::value::NodeValueRow::from([(TEXTURE_INPUT.to_string(), tex())]);
		let payload = run(&core, &inputs);
		assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.vhsrevrb");
		assert_eq!(payload.effect_input, TEXTURE_INPUT);
		assert_eq!(
			payload.params.get(VHS_BLEED_INPUT),
			Some(&NodeValue::Float(0.9))
		);
		for (id, _, _) in SLIDERS {
			assert!(payload.params.contains_key(id), "{id} missing");
		}
		assert!(payload.params.contains_key("time_in"));
	}

	#[test]
	fn shader_declares_every_uniform_and_avoids_switch() {
		let glsl = VhsRevrbNode.shader_code("").unwrap();
		for (id, _, _) in SLIDERS {
			assert!(glsl.contains(&format!("uniform float {id};")), "{id}");
		}
		assert!(glsl.contains("uniform float time_in;"));
		assert!(glsl.contains("uniform vec2 resolution_in;"));
		assert!(!glsl.contains("switch"));
	}
}
