use operational_transform::OperationSeq;

pub trait IndexTransformer {
    fn transform_index(&self, position: u32) -> u32;
}

impl IndexTransformer for OperationSeq {
    fn transform_index(&self, position: u32) -> u32 {
        let mut index = position as i32;
        let mut new_index = index;
        for op in self.ops() {
            use operational_transform::Operation::*;
            match op {
                &Retain(n) => index -= n as i32,
                Insert(s) => new_index += bytecount::num_chars(s.as_bytes()) as i32,
                &Delete(n) => {
                    new_index -= std::cmp::min(index, n as i32);
                    index -= n as i32;
                }
            }
            if index < 0 {
                break;
            }
        }
        new_index as u32
    }
}
