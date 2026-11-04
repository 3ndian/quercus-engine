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

//! Minimal Windows GL probe: run this on the target machine before
//! expecting OpenFX zero-copy to work.
//!
//! ```text
//! cargo run -p quercus-ofx-plugin --example win_gl_probe
//! ```
//!
//! It prints the WGL context's `GL_VERSION` / `GL_RENDERER` and whether
//! `GL_EXT_memory_object_win32` functions are loadable. If
//! `memory_object=false`, this GPU/driver cannot use the Windows
//! zero-copy path (the engine falls back to the old GL readback).

fn main() {
	println!(
		"gl_available={}",
		quercus_ofx_plugin::gl_bridge::gl_available()
	);
	println!(
		"gl_version={:?}",
		quercus_ofx_plugin::gl_bridge::gl_version_string()
	);
	println!(
		"memory_object={}",
		quercus_ofx_plugin::gl_bridge::gl_memory_object_available()
	);
	#[cfg(target_os = "windows")]
	println!(
		"platform=windows (requires Vulkan backend + VK_KHR_external_memory_win32 for zero-copy)"
	);
	#[cfg(target_os = "linux")]
	println!("platform=linux");
	#[cfg(target_os = "macos")]
	println!("platform=macos (zero-copy GL not implemented)");
}
