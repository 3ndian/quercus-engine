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

//! SubRip (`.srt`) reading/writing built on the maintained `subparse` crate.
//!
//! The crate only sees a decoded `&str`, so UTF-8 (with or without BOM) and
//! UTF-16 LE/BE with BOM keep working exactly as before. The document is
//! split into blank-line-separated cue blocks and each block is handed to
//! `subparse`'s SRT grammar. A block that `subparse` rejects (bad index,
//! timing line, etc.) is skipped rather than failing the whole document,
//! matching the previous parser's malformed-input behaviour; only an
//! undecodable byte stream makes `parse_srt` return `Err`. Empty input
//! yields an empty cue list and never panics. HTML style tags are preserved
//! verbatim, and `subparse` trims surrounding whitespace from each text line.

use subparse::timetypes::{TimePoint, TimeSpan};
use subparse::{SrtFile, SubtitleFileInterface};

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

/// Split a decoded SRT document into blank-line-separated cue blocks.
///
/// Lone `\r` line endings are normalized to `\n` (Rust's `str::lines` only
/// understands `\n` and `\r\n`).
fn cue_blocks(text: &str) -> Vec<String> {
	let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
	let mut blocks = Vec::new();
	let mut block: Vec<&str> = Vec::new();
	for line in normalized.lines().chain(std::iter::once("")) {
		if line.trim().is_empty() {
			if !block.is_empty() {
				blocks.push(block.join("\n"));
				block.clear();
			}
		} else {
			block.push(line);
		}
	}
	blocks
}

/// Parse an SRT document into cues, preserving multi-line cue text.
///
/// Malformed cue blocks are skipped: a block rejected by `subparse` does not
/// abort the rest of the document. Only decoding failures return `Err`.
pub fn parse_srt(bytes: &[u8]) -> Result<Vec<SubtitleCue>, String> {
	let text = decode_text(bytes)?;
	let mut cues = Vec::new();
	for block in cue_blocks(&text) {
		// A malformed block (e.g. bad index/timing line) is skipped so one
		// bad cue cannot discard the rest of the document.
		let Ok(file) = SrtFile::parse(&block) else {
			continue;
		};
		let Ok(entries) = file.get_subtitle_entries() else {
			continue;
		};
		for entry in entries {
			let in_seconds = entry.timespan.start.secs_f64();
			let out_seconds = entry.timespan.end.secs_f64();
			if out_seconds <= in_seconds {
				continue;
			}
			let Some(text) = entry.line else {
				continue;
			};
			if text.trim().is_empty() {
				continue;
			}
			cues.push(SubtitleCue {
				in_seconds,
				out_seconds,
				text,
			});
		}
	}
	Ok(cues)
}

/// Convert non-negative seconds to whole milliseconds, rounding like the
/// previous writer did.
fn seconds_to_msecs(seconds: f64) -> i64 {
	(seconds.max(0.0) * 1000.0).round() as i64
}

/// Serialize cues as a valid SRT document (1-based indices, LF line endings).
///
/// Uses `subparse`'s SRT serializer. Cue text keeps its multiple lines; the
/// trailing whitespace trimmed by the previous writer is still trimmed so
/// existing callers see the same document.
pub fn write_srt(cues: &[SubtitleCue]) -> String {
	let entries: Vec<(TimeSpan, String)> = cues
		.iter()
		.map(|cue| {
			let start = TimePoint::from_msecs(seconds_to_msecs(cue.in_seconds));
			let end = TimePoint::from_msecs(seconds_to_msecs(cue.out_seconds));
			(TimeSpan::new(start, end), cue.text.trim_end().to_string())
		})
		.collect();
	let file = SrtFile::create(entries).expect("subparse always creates an SRT file");
	let data = file.to_data().expect("subparse always serializes SRT data");
	String::from_utf8(data).expect("subparse serializes SRT as UTF-8")
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
		assert!(parse_srt(b"1\n00:00:02,000 --> 00:00:01,000\nx\n\n")
			.unwrap()
			.is_empty());
		let text = "1\n00:00:00,000 --> 00:00:01,000\nHi\n";
		let mut utf16 = vec![0xFF, 0xFE];
		for unit in text.encode_utf16() {
			utf16.extend_from_slice(&unit.to_le_bytes());
		}
		assert_eq!(parse_srt(&utf16).unwrap()[0].text, "Hi");
	}

	#[test]
	fn parses_utf16le_bom_multiline_and_chinese() {
		let text = "1\r\n00:00:06,000 --> 00:00:07,250\r\n你好\r\n世界\r\n";
		let mut utf16 = vec![0xFF, 0xFE];
		for unit in text.encode_utf16() {
			utf16.extend_from_slice(&unit.to_le_bytes());
		}
		let cues = parse_srt(&utf16).unwrap();
		assert_eq!(cues.len(), 1);
		assert_eq!(cues[0].in_seconds, 6.0);
		assert_eq!(cues[0].out_seconds, 7.25);
		assert_eq!(cues[0].text, "你好\n世界");
	}

	#[test]
	fn parses_crlf_and_multiline_text() {
		let input = b"1\r\n00:00:01,000 --> 00:00:02,000\r\nFirst line\r\nSecond line\r\n\r\n2\r\n00:00:03,000 --> 00:00:04,000\r\n\xE7\xAC\xAC\xE4\xB8\x89\xE8\xA1\x8C\r\n";
		let cues = parse_srt(input).unwrap();
		assert_eq!(cues.len(), 2);
		assert_eq!(cues[0].in_seconds, 1.0);
		assert_eq!(cues[0].out_seconds, 2.0);
		assert_eq!(cues[0].text, "First line\nSecond line");
		assert_eq!(cues[1].text, "第三行");
		let written = write_srt(&cues);
		assert!(written.contains("First line\nSecond line"));
		assert!(written.contains("第三行"));
		assert_eq!(parse_srt(written.as_bytes()).unwrap(), cues);
	}

	#[test]
	fn malformed_cue_is_skipped_not_fatal() {
		// The middle block has no valid timing line, so `subparse` rejects it;
		// the surrounding good cues must still be returned.
		let input = b"1\n00:00:01,000 --> 00:00:02,000\nGood\n\n2\nnot a timing line\nBad\n\n3\n00:00:03,000 --> 00:00:04,000\nStill good\n";
		let cues = parse_srt(input).unwrap();
		assert_eq!(cues.len(), 2);
		assert_eq!(cues[0].text, "Good");
		assert_eq!(cues[1].text, "Still good");
		assert!(parse_srt(b"").unwrap().is_empty());
		assert!(parse_srt(b"\n\n\n").unwrap().is_empty());
	}

	#[test]
	fn write_srt_roundtrips_empty_and_multiline() {
		assert_eq!(write_srt(&[]), "");
		let cues = vec![SubtitleCue {
			in_seconds: 0.0,
			out_seconds: 1.234,
			text: "a\nb".to_string(),
		}];
		let out = write_srt(&cues);
		assert_eq!(out, "1\n00:00:00,000 --> 00:00:01,234\na\nb\n\n");
		assert_eq!(parse_srt(out.as_bytes()).unwrap(), cues);
	}
}
