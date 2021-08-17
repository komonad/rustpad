use xi_rope::Rope;
use crate::code_editor::text::{EditableText, StringCursor};
use std::ops::Range;
use std::borrow::Cow;

impl EditableText for Rope {
    fn cursor(&self, position: usize) -> Option<StringCursor> {

    }

    fn edit(&mut self, range: Range<usize>, new: impl Into<String>) {
        todo!()
    }

    fn slice(&self, range: Range<usize>) -> Option<Cow<str>> {
        todo!()
    }

    fn len(&self) -> usize {
        todo!()
    }

    fn prev_word_offset(&self, offset: usize) -> Option<usize> {
        todo!()
    }

    fn next_word_offset(&self, offset: usize) -> Option<usize> {
        todo!()
    }

    fn prev_grapheme_offset(&self, offset: usize) -> Option<usize> {
        todo!()
    }

    fn next_grapheme_offset(&self, offset: usize) -> Option<usize> {
        todo!()
    }

    fn prev_codepoint_offset(&self, offset: usize) -> Option<usize> {
        todo!()
    }

    fn next_codepoint_offset(&self, offset: usize) -> Option<usize> {
        todo!()
    }

    fn preceding_line_break(&self, offset: usize) -> usize {
        todo!()
    }

    fn next_line_break(&self, offset: usize) -> usize {
        todo!()
    }

    fn is_empty(&self) -> bool {
        todo!()
    }

    fn from_str(s: &str) -> Self {
        todo!()
    }
}
