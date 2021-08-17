use crate::client::{Callback, RustpadClient};
use std::borrow::Cow;
use std::ops::Range;
use druid::piet::TextStorage as PietTextStorage;
use druid::Data;
use std::sync::Arc;
use parking_lot::RwLock;
use crate::code_editor::text::{TextStorage, StringCursor};
use crate::code_editor::text::editable_text::EditableText;
use rustpad_server::rustpad::CursorData;
// use druid::text::{EditableText, TextStorage, StringCursor};


#[derive(Default, Debug, Clone)]
pub struct Edit {
    pub begin: usize,
    pub end: usize,
    pub content: String,
}

pub struct Edit1(Edit);

impl Drop for Edit1 {
    fn drop(&mut self) {
        println!("{:?} dropped", self.0);
    }
}

pub trait EditorProxy: Send + Sync {
    fn edit_content(&mut self, edit: Edit);
    fn get_content(&self) -> String;
    fn on_edit(&mut self, callback: Callback<*const Edit>);
}

#[derive(Default, Clone)]
pub struct EditorBinding {
    pub content: Vec<char>,
    pub content_as_string: String,
    pub after_edits: Vec<Callback<*const Edit>>,
}

#[derive(Clone)]
struct EditorBindingPtr(Arc<RwLock<EditorBinding>>, String);

impl EditorBinding {
    fn calculate_content_as_string(&self) -> String {
        self.content.iter().collect::<String>()
    }

    fn update_content_as_string(&mut self) {
        self.content_as_string = self.calculate_content_as_string();
    }

    pub fn edit_without_callback(&mut self, edit: &Edit) {
        println!("start edit {:?}", edit);

        // apply edit
        let Edit { begin, end, content } = edit.clone();

        self.content.drain(begin .. end);
        let mut counter = 0;
        for c in content.chars() {
            self.content.insert(begin + counter, c);
            counter += 1;
        }

        self.update_content_as_string();

        println!("current content: {}", self.content_as_string);
    }

    pub fn set_client(&mut self, client: &Arc<RwLock<RustpadClient>>) {
        let copy = Arc::clone(&client);
        self.on_edit(Callback::new(Arc::new(RwLock::new(move |b: *const Edit| unsafe {
            // GUI edit to Rustpad Client
            if let Some(mut handle) = copy.try_write() {
                handle.editor_binding.edit_without_callback(&*b);
                handle.on_change((*b).clone());
            }
        }))));
    }

    pub fn create_with_client(client: &Arc<RwLock<RustpadClient>>) -> Self {
        let mut our_document = EditorBinding::default();
        our_document.set_client(client);
        our_document
    }
}

impl EditorProxy for EditorBinding {
    fn edit_content(&mut self, edit: Edit) {
        self.edit_without_callback(&edit);

        //  callback
        let edit_ptr: *const Edit = &edit;
        for x in self.after_edits.iter() {
            x.invoke(edit_ptr);
        }
    }

    fn get_content(&self) -> String {
        self.content_as_string.clone()
    }

    fn on_edit(&mut self, callback: Callback<*const Edit>) {
        self.after_edits.push(callback);
    }

}

impl PietTextStorage for EditorBinding {
    fn as_str(&self) -> &str {
        self.content_as_string.as_str()
    }
}

impl Data for EditorBinding {
    fn same(&self, other: &Self) -> bool {
        self.content == other.content
    }
}

impl TextStorage for EditorBinding {
    // TODO
}

impl EditableText for EditorBinding {
    fn cursor(&self, position: usize) -> Option<StringCursor> {
        self.content_as_string.cursor(position)
    }

    fn edit(&mut self, range: Range<usize>, new: impl Into<String>) {
        let code_point_begin = bytecount::num_chars(&self.content_as_string.as_bytes()[..range.start]);
        let code_point_end = code_point_begin + bytecount::num_chars(&self.content_as_string.as_bytes()[range.start .. range.end]);
        self.edit_content(Edit {
            begin: code_point_begin,
            end: code_point_end,
            content: new.into()
        });
    }

    fn slice(&self, range: Range<usize>) -> Option<Cow<str>> {
        self.content_as_string.slice(range)
    }

    fn len(&self) -> usize {
        self.content_as_string.len()
    }

    fn prev_word_offset(&self, offset: usize) -> Option<usize> {
        self.content_as_string.next_word_offset(offset)
    }

    fn next_word_offset(&self, offset: usize) -> Option<usize> {
        self.content_as_string.next_word_offset(offset)
    }

    fn prev_grapheme_offset(&self, offset: usize) -> Option<usize> {
        self.content_as_string.prev_grapheme_offset(offset)
    }

    fn next_grapheme_offset(&self, offset: usize) -> Option<usize> {
        self.content_as_string.next_grapheme_offset(offset)
    }

    fn prev_codepoint_offset(&self, offset: usize) -> Option<usize> {
        self.content_as_string.prev_codepoint_offset(offset)
    }

    fn next_codepoint_offset(&self, offset: usize) -> Option<usize> {
        self.content_as_string.next_codepoint_offset(offset)
    }

    fn preceding_line_break(&self, offset: usize) -> usize {
        self.content_as_string.preceding_line_break(offset)
    }

    fn next_line_break(&self, offset: usize) -> usize {
        self.content_as_string.next_line_break(offset)
    }

    fn is_empty(&self) -> bool {
        self.content.is_empty()
    }

    fn from_str(s: &str) -> Self {
        EditorBinding {
            content: s.chars().collect(),
            content_as_string: s.to_string(),
            after_edits: vec![]
        }
    }
}
//
// impl PietTextStorage for EditorBindingPtr {
//     fn as_str(&self) -> &str {
//         self.0.read().content_as_string.as_str()
//     }
// }
//
// impl Data for EditorBindingPtr {
//     fn same(&self, other: &Self) -> bool {
//         self.0.read().same(other.0.read().deref())
//     }
// }
//
// impl TextStorage for EditorBindingPtr {
//
// }
//
// impl EditableText for EditorBindingPtr {
//     fn cursor(&self, position: usize) -> Option<StringCursor> {
//         todo!()
//     }
//
//     fn edit(&mut self, range: Range<usize>, new: impl Into<String>) {
//         todo!()
//     }
//
//     fn slice(&self, range: Range<usize>) -> Option<Cow<str>> {
//         todo!()
//     }
//
//     fn len(&self) -> usize {
//         todo!()
//     }
//
//     fn prev_word_offset(&self, offset: usize) -> Option<usize> {
//         todo!()
//     }
//
//     fn next_word_offset(&self, offset: usize) -> Option<usize> {
//         todo!()
//     }
//
//     fn prev_grapheme_offset(&self, offset: usize) -> Option<usize> {
//         todo!()
//     }
//
//     fn next_grapheme_offset(&self, offset: usize) -> Option<usize> {
//         todo!()
//     }
//
//     fn prev_codepoint_offset(&self, offset: usize) -> Option<usize> {
//         todo!()
//     }
//
//     fn next_codepoint_offset(&self, offset: usize) -> Option<usize> {
//         todo!()
//     }
//
//     fn preceding_line_break(&self, offset: usize) -> usize {
//         todo!()
//     }
//
//     fn next_line_break(&self, offset: usize) -> usize {
//         todo!()
//     }
//
//     fn is_empty(&self) -> bool {
//         todo!()
//     }
//
//     fn from_str(s: &str) -> Self {
//         todo!()
//     }
// }
