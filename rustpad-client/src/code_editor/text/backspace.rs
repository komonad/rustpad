// Copyright 2019 The Druid Authors.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Calc start of a backspace delete interval

use super::editable_text::{EditableText, EditableTextCursor};
use druid_shell::text::Selection;

use xi_unicode::*;

#[allow(clippy::cognitive_complexity)]
fn backspace_offset(text: &impl EditableText, start: usize) -> usize {
    #[derive(PartialEq)]
    enum State {
        Start,
        Lf,
        BeforeKeycap,
        BeforeVsAndKeycap,
        BeforeEmojiModifier,
        BeforeVsAndEmojiModifier,
        BeforeVs,
        BeforeEmoji,
        BeforeZwj,
        BeforeVsAndZwj,
        OddNumberedRis,
        EvenNumberedRis,
        InTagSequence,
        Finished,
    }
    let mut state = State::Start;

    let mut delete_code_point_count = 0;
    let mut last_seen_vs_code_point_count = 0;
    let mut cursor = text
        .cursor(start)
        .expect("Backspace must begin at a valid codepoint boundary.");

    while state != State::Finished && cursor.pos() > 0 {
        let code_point = cursor.prev_codepoint().unwrap_or('0');

        match state {
            State::Start => {
                delete_code_point_count = 1;
                if code_point == '\n' {
                    state = State::Lf;
                } else if is_variation_selector(code_point) {
                    state = State::BeforeVs;
                } else if code_point.is_regional_indicator_symbol() {
                    state = State::OddNumberedRis;
                } else if code_point.is_emoji_modifier() {
                    state = State::BeforeEmojiModifier;
                } else if code_point.is_emoji_combining_enclosing_keycap() {
                    state = State::BeforeKeycap;
                } else if code_point.is_emoji() {
                    state = State::BeforeEmoji;
                } else if code_point.is_emoji_cancel_tag() {
                    state = State::InTagSequence;
                } else {
                    state = State::Finished;
                }
            }
            State::Lf => {
                if code_point == '\r' {
                    delete_code_point_count += 1;
                }
                state = State::Finished;
            }
            State::OddNumberedRis => {
                if code_point.is_regional_indicator_symbol() {
                    delete_code_point_count += 1;
                    state = State::EvenNumberedRis
                } else {
                    state = State::Finished
                }
            }
            State::EvenNumberedRis => {
                if code_point.is_regional_indicator_symbol() {
                    delete_code_point_count -= 1;
                    state = State::OddNumberedRis;
                } else {
                    state = State::Finished;
                }
            }
            State::BeforeKeycap => {
                if is_variation_selector(code_point) {
                    last_seen_vs_code_point_count = 1;
                    state = State::BeforeVsAndKeycap;
                } else {
                    if is_keycap_base(code_point) {
                        delete_code_point_count += 1;
                    }
                    state = State::Finished;
                }
            }
            State::BeforeVsAndKeycap => {
                if is_keycap_base(code_point) {
                    delete_code_point_count += last_seen_vs_code_point_count + 1;
                }
                state = State::Finished;
            }
            State::BeforeEmojiModifier => {
                if is_variation_selector(code_point) {
                    last_seen_vs_code_point_count = 1;
                    state = State::BeforeVsAndEmojiModifier;
                } else {
                    if code_point.is_emoji_modifier_base() {
                        delete_code_point_count += 1;
                    }
                    state = State::Finished;
                }
            }
            State::BeforeVsAndEmojiModifier => {
                if code_point.is_emoji_modifier_base() {
                    delete_code_point_count += last_seen_vs_code_point_count + 1;
                }
                state = State::Finished;
            }
            State::BeforeVs => {
                if code_point.is_emoji() {
                    delete_code_point_count += 1;
                    state = State::BeforeEmoji;
                } else {
                    if !is_variation_selector(code_point) {
                        //TODO: UCharacter.getCombiningClass(codePoint) == 0
                        delete_code_point_count += 1;
                    }
                    state = State::Finished;
                }
            }
            State::BeforeEmoji => {
                if code_point.is_zwj() {
                    state = State::BeforeZwj;
                } else {
                    state = State::Finished;
                }
            }
            State::BeforeZwj => {
                if code_point.is_emoji() {
                    delete_code_point_count += 2;
                    state = if code_point.is_emoji_modifier() {
                        State::BeforeEmojiModifier
                    } else {
                        State::BeforeEmoji
                    };
                } else if is_variation_selector(code_point) {
                    last_seen_vs_code_point_count = 1;
                    state = State::BeforeVsAndZwj;
                } else {
                    state = State::Finished;
                }
            }
            State::BeforeVsAndZwj => {
                if code_point.is_emoji() {
                    delete_code_point_count += last_seen_vs_code_point_count + 2;
                    last_seen_vs_code_point_count = 0;
                    state = State::BeforeEmoji;
                } else {
                    state = State::Finished;
                }
            }
            State::InTagSequence => {
                if code_point.is_tag_spec_char() {
                    delete_code_point_count += 1;
                } else if code_point.is_emoji() {
                    delete_code_point_count += 1;
                    state = State::Finished;
                } else {
                    delete_code_point_count = 1;
                    state = State::Finished;
                }
            }
            State::Finished => {
                break;
            }
        }
    }

    cursor.set(start);
    for _ in 0..delete_code_point_count {
        let _ = cursor.prev_codepoint();
    }
    cursor.pos()
}

/// Calculate resulting offset for a backwards delete.
///
/// This involves complicated logic to handle various special cases that
/// are unique to backspace.
#[allow(clippy::trivially_copy_pass_by_ref)]
pub fn offset_for_delete_backwards(region: &Selection, text: &impl EditableText) -> usize {
    if !region.is_caret() {
        region.min()
    } else {
        backspace_offset(text, region.active)
    }
}
