use cuid2::cuid;

#[inline]
#[allow(dead_code)]
pub fn generate_id<T: Into<String>>(r#type: T) -> String {
    let id = cuid();
    format!("{}_{}", r#type.into(), id)
}
