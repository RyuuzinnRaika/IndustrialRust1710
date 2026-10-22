//! Validated 1.7.10 writable and signed book payloads.
//! Inventory slot NBT and the Protocol 5 book-edit packet still need integration.
use serde::{Deserialize, Serialize};

pub const MAX_PAGES: usize = 50;
pub const MAX_PAGE_CHARS: usize = 256;
pub const MAX_TITLE_CHARS: usize = 16;
pub const MAX_AUTHOR_CHARS: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Book {
    Writable { pages: Vec<String> },
    Signed { title: String, author: String, pages: Vec<String> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BookError {
    TooManyPages,
    PageTooLong,
    InvalidTitle,
    InvalidAuthor,
    AlreadySigned,
}

fn valid_pages(pages: &[String]) -> Result<(), BookError> {
    if pages.len() > MAX_PAGES { return Err(BookError::TooManyPages); }
    if pages.iter().any(|p| p.chars().count() > MAX_PAGE_CHARS) {
        return Err(BookError::PageTooLong);
    }
    Ok(())
}

impl Book {
    pub fn writable() -> Self { Self::Writable { pages: vec![String::new()] } }

    pub fn edit(&mut self, pages: Vec<String>) -> Result<(), BookError> {
        valid_pages(&pages)?;
        match self {
            Self::Writable { pages: existing } => { *existing = pages; Ok(()) }
            Self::Signed { .. } => Err(BookError::AlreadySigned),
        }
    }

    /// Returns a signed copy. No mutation occurs if any field is invalid.
    pub fn sign(&self, title: String, author: String) -> Result<Self, BookError> {
        let pages = match self {
            Self::Writable { pages } => pages,
            Self::Signed { .. } => return Err(BookError::AlreadySigned),
        };
        if title.is_empty() || title.chars().count() > MAX_TITLE_CHARS
            || title.chars().any(char::is_control) {
            return Err(BookError::InvalidTitle);
        }
        if author.is_empty() || author.chars().count() > MAX_AUTHOR_CHARS
            || author.chars().any(char::is_control) {
            return Err(BookError::InvalidAuthor);
        }
        valid_pages(pages)?;
        Ok(Self::Signed { title, author, pages: pages.clone() })
    }

    pub fn validate(&self) -> Result<(), BookError> {
        match self {
            Self::Writable { pages } => valid_pages(pages),
            Self::Signed { title, author, pages } => {
                let original = Self::Writable { pages: pages.clone() };
                original.sign(title.clone(), author.clone()).map(|_| ())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn edits_then_signs() {
        let mut book = Book::writable();
        book.edit(vec!["hello".into(), "world".into()]).unwrap();
        let mut signed = book.sign("Diary".into(), "Player".into()).unwrap();
        assert!(signed.edit(vec!["changed".into()]).is_err());
        signed.validate().unwrap();
    }
    #[test]
    fn limits_enforced() {
        let mut book = Book::writable();
        assert_eq!(book.edit(vec!["x".repeat(257)]), Err(BookError::PageTooLong));
        assert_eq!(book.edit(vec![String::new(); 51]), Err(BookError::TooManyPages));
        assert_eq!(book.sign("".into(), "Player".into()), Err(BookError::InvalidTitle));
    }
}
