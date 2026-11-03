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

//! Sequence master-bus audio effect chain commands.
//!
//! The chain is stored in the project's settings map under
//! [`AUDIO_BUS_EFFECTS_SETTING`], keyed by sequence identity, so preview,
//! export and the render worker read the same list and a project save
//! persists it. There is no UI for it yet; the C ABI exposes the same
//! add/remove/enable/parameter operations as the clip chain.

use super::media::push_command;
use super::prelude::*;
use super::project::lock;
use quercus_node::value::NodeValue;
use quercus_render::audio::{load_bus_effects, store_bus_effects, AUDIO_BUS_EFFECTS_SETTING};
use quercus_render::ticket::MontageAudioEffect;

fn set_bus_setting(project: &ProjectRef, value: Option<String>) {
	let mut guard = lock(project);
	match value {
		Some(value) => {
			guard
				.settings
				.insert(AUDIO_BUS_EFFECTS_SETTING.to_string(), value);
		}
		None => {
			guard.settings.remove(AUDIO_BUS_EFFECTS_SETTING);
		}
	}
	guard.set_modified(true);
}

/// Read the sequence's current master-bus chain.
pub fn audio_bus_effects(project: &ProjectRef, sequence: NodeId) -> Vec<MontageAudioEffect> {
	if !sequence.valid() {
		return Vec::new();
	}
	let guard = lock(project);
	load_bus_effects(&guard.settings, sequence.identity())
}

/// Apply `update` to a cloned bus chain and push one undo row.
///
/// The project-settings value is captured in both directions, so undo
/// restores the previous JSON (including all other sequences' chains).
fn update_bus_effects(
	project: &ProjectRef,
	sequence: NodeId,
	label: &'static str,
	update: impl FnOnce(&mut Vec<MontageAudioEffect>) -> Result<bool, String>,
) -> Result<(), String> {
	if !sequence.valid() {
		return Err("the session has no current sequence".into());
	}
	let (old_value, new_value) = {
		let guard = lock(project);
		if !guard
			.graph
			.get(sequence)
			.and_then(|entry| entry.behavior.as_any())
			.and_then(|behavior| behavior.downcast_ref::<SequenceBehavior>())
			.is_some()
		{
			return Err("the current node is not a sequence".into());
		}
		let old_value = guard.settings.get(AUDIO_BUS_EFFECTS_SETTING).cloned();
		let mut effects = load_bus_effects(&guard.settings, sequence.identity());
		if !update(&mut effects)? {
			return Ok(());
		}
		let mut settings = guard.settings.clone();
		store_bus_effects(&mut settings, sequence.identity(), &effects)?;
		(old_value, settings.get(AUDIO_BUS_EFFECTS_SETTING).cloned())
	};
	if old_value == new_value {
		return Ok(());
	}
	let redo_project = project.clone();
	let undo_project = project.clone();
	push_command(
		quercus_undo::undocommand::UndoCommand::from_closures(
			move || set_bus_setting(&redo_project, new_value.clone()),
			move || set_bus_setting(&undo_project, old_value.clone()),
		),
		label,
	)
}

/// Append a bus effect at `index` (clamped to the chain length) as one
/// undo row. The type id is carried even when the renderer does not know
/// it yet; unknown entries are bypassed with a warning.
pub fn add_audio_bus_effect(
	project: &ProjectRef,
	sequence: NodeId,
	type_id: &str,
	index: usize,
) -> Result<(), String> {
	let type_id = type_id.trim();
	if type_id.is_empty() {
		return Err("audio bus effect type id must not be empty".into());
	}
	let effect = MontageAudioEffect {
		type_id: type_id.to_string(),
		enabled: true,
		params: Vec::new(),
	};
	update_bus_effects(project, sequence, "Add Master Audio Effect", |effects| {
		let index = index.min(effects.len());
		effects.insert(index, effect);
		Ok(true)
	})
}

/// Remove one bus effect as one undo row.
pub fn remove_audio_bus_effect(
	project: &ProjectRef,
	sequence: NodeId,
	index: usize,
) -> Result<(), String> {
	update_bus_effects(project, sequence, "Remove Master Audio Effect", |effects| {
		if index >= effects.len() {
			return Err("audio bus effect index out of range".into());
		}
		effects.remove(index);
		Ok(true)
	})
}

/// Enable/disable one bus effect as one undo row.
pub fn set_audio_bus_effect_enabled(
	project: &ProjectRef,
	sequence: NodeId,
	index: usize,
	enabled: bool,
) -> Result<(), String> {
	update_bus_effects(
		project,
		sequence,
		"Set Master Audio Effect Enabled",
		|effects| {
			let effect = effects
				.get_mut(index)
				.ok_or_else(|| "audio bus effect index out of range".to_string())?;
			if effect.enabled == enabled {
				return Ok(false);
			}
			effect.enabled = enabled;
			Ok(true)
		},
	)
}

/// Set one static float parameter of a bus effect as one undo row.
pub fn set_audio_bus_effect_param(
	project: &ProjectRef,
	sequence: NodeId,
	index: usize,
	input_id: &str,
	value: f64,
) -> Result<(), String> {
	let input_id = input_id.trim();
	if input_id.is_empty() {
		return Err("audio bus effect input id must not be empty".into());
	}
	if !value.is_finite() {
		return Err("audio bus effect parameter must be finite".into());
	}
	let input_id = input_id.to_string();
	update_bus_effects(
		project,
		sequence,
		"Set Master Audio Effect Param",
		|effects| {
			let effect = effects
				.get_mut(index)
				.ok_or_else(|| "audio bus effect index out of range".to_string())?;
			if effect.param(&input_id) == Some(&NodeValue::Float(value)) {
				return Ok(false);
			}
			effect.params.retain(|(input, _)| input != &input_id);
			effect
				.params
				.push((input_id.clone(), NodeValue::Float(value)));
			Ok(true)
		},
	)
}

/// Read one static scalar bus-effect parameter.
pub fn audio_bus_effect_param(
	project: &ProjectRef,
	sequence: NodeId,
	index: usize,
	input_id: &str,
) -> Option<f64> {
	audio_bus_effects(project, sequence)
		.get(index)
		.and_then(|effect| effect.param(input_id))
		.map(|value| value.to_double())
}
