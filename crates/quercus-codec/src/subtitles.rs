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

//! Minimal SubRip (`.srt`) parser/writer used by the subtitle workflow.
//!
//! The parser accepts the common SRT dialect: a numeric index, a
//! `HH:MM:SS,mmm --> HH:MM:SS,mmm` line, one or more text lines, and a blank
//! separator. UTF-8 (with or without BOM) and UTF-16 LE/BE with BOM are
//! accepted; HTML style tags are preserved verbatim as text.

/// One subtitle cue with second-based times.
#[derive(Clone, Debug, PartialEq)]
pub struct SubtitleCue {
	/// Cue in-point in seconds.
	pub in_seconds: f64,
	/// Cue out-point in seconds.
	pub out_seconds: f64,
	/// Cue text (may contain newlines).
	pub text: String,
}

fn decode_text(bytes: &[u8]) -> Result<String, String> {
	if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
		return String::from_utf8(bytes[3..].to_vec()).map_err(|_| "SRT is not UTF-8".into());
	}
	if bytes.starts_with(&[0xFF, 0xFE]) {
		let units: Vec<u16> = bytes[2..]
			.chunks_exact(2)
			.map(|c| u16::from_le_bytes([c[0], c[1]]))
			.collect();
		return String::from_utf16(&units).map_err(|_| "SRT is not valid UTF-16LE".into());
	}
	if bytes.starts_with(&[0xFE, 0xFF]) {
		let units: Vec<u16> = bytes[2..]
			.chunks_exact(2)
			.map(|c| u16::from_be_bytes([c[0], c[1]]))
			.collect();
		return String::from_utf16(&units).map_err(|_| "SRT is not valid UTF-16BE".into());
	}
	String::from_utf8(bytes.to_vec()).map_err(|_| "SRT is not UTF-8".into())
}

fn parse_timestamp(text: &str) -> Option<f64> {
	let text = text.trim().replace(',', ".");
	let parts: Vec<&str> = text.split(':').collect();
	if parts.len() != 3 {
		return None;
	}
	let hours: f64 = parts[0].parse().ok()?;
	let minutes: f64 = parts[1].parse().ok()?;
	let seconds: f64 = parts[2].parse().ok()?;
	if !(0.0..60.0).contains(&minutes) || !(0.0..61.0).contains(&seconds) {
		return None;
	}
	Some(hours * 3600.0 + minutes * 60.0 + seconds)
}

/// Parse an SRT document into cues, preserving multi-line cue text.
pub fn parse_srt(bytes: &[u8]) -> Result<Vec<SubtitleCue>, String> {
	let text = decode_text(bytes)?;
	let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
	let mut cues = Vec::new();
	let mut block: Vec<&str> = Vec::new();
	for line in normalized.lines().chain(std::iter::once("")) {
		if line.trim().is_empty() {
			if block.is_empty() {
				continue;
			}
			let mut index = 0;
			if block
				.first()
				.is_some_and(|line| line.trim().parse::<u64>().is_ok())
			{
				index = 1;
			}
			let Some(timing) = block.get(index) else {
				block.clear();
				continue;
			};
			let Some((start, end)) = timing.split_once("-->") else {
				block.clear();
				continue;
			};
			let (Some(in_seconds), Some(out_seconds)) =
				(parse_timestamp(start), parse_timestamp(end))
			else {
				block.clear();
				continue;
			};
			if out_seconds <= in_seconds {
				block.clear();
				continue;
			}
			let body = block[index + 1..].join("\n");
			if body.trim().is_empty() {
				block.clear();
				continue;
			}
			cues.push(SubtitleCue {
				in_seconds,
				out_seconds,
				text: body,
			});
			block.clear();
		} else {
			block.push(line);
		}
	}
	Ok(cues)
}

fn format_timestamp(seconds: f64) -> String {
	let total_ms = (seconds.max(0.0) * 1000.0).round() as u64;
	let hours = total_ms / 3_600_000;
	let minutes = (total_ms / 60_000) % 60;
	let secs = (total_ms / 1000) % 60;
	let millis = total_ms % 1000;
	format!("{hours:02}:{minutes:02}:{secs:02},{millis:03}")
}

/// Serialize cues as a valid SRT document (1-based indices, CRLF-free).
pub fn write_srt(cues: &[SubtitleCue]) -> String {
	let mut out = String::new();
	for (index, cue) in cues.iter().enumerate() {
		out.push_str(&(index + 1).to_string());
		out.push('\n');
		out.push_str(&format!(
			"{} --> {}\n",
			format_timestamp(cue.in_seconds),
			format_timestamp(cue.out_seconds)
		));
		out.push_str(cue.text.trim_end());
		out.push_str("\n\n");
	}
	out
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_bom_multiline_and_writes_back() {
		let input = b"\xEF\xBB\xBF1\r\n00:00:01,500 --> 00:00:03,000\r\nHello\r\nWorld\r\n\r\n2\n00:00:04,000 --> 00:00:05,000\n\xE4\xB8\xAD\xE6\x96\x87\n";
		let cues = parse_srt(input).unwrap();
		assert_eq!(cues.len(), 2);
		assert_eq!(cues[0].in_seconds, 1.5);
		assert_eq!(cues[0].out_seconds, 3.0);
		assert_eq!(cues[0].text, "Hello\nWorld");
		assert_eq!(cues[1].text, "中文");
		let out = write_srt(&cues);
		assert!(out.contains("00:00:01,500 --> 00:00:03,000"));
		let round = parse_srt(out.as_bytes()).unwrap();
		assert_eq!(round, cues);
	}

	#[test]
	fn rejects_malformed_and_utf16_is_supported() {
		assert!(parse_srt(b"not an srt").unwrap().is_empty());
		assert!(parse_srt(b"1\n00:00:02,000 --> 00:00:01,000\nx\n\n").unwrap().is_empty());
		let text = "1\n00:00:00,000 --> 00:00:01,000\nHi\n";
		let mut utf16 = vec![0xFF, 0xFE];
		for unit in text.encode_utf16() {
			utf16.extend_from_slice(&unit.to_le_bytes());
		}
		assert_eq!(parse_srt(&utf16).unwrap()[0].text, "Hi");
	}
}
