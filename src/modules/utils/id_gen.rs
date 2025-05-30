use cuid2::cuid;

#[inline]
pub fn generate_id<T : Into<String>>(r#type: T) -> String {
    let id = cuid();
    format!("{}_{}", r#type.into(), id)
}