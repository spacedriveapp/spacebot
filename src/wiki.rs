//! Instance-wide wiki knowledge base.

mod store;

pub use store::{
    CreateWikiPageInput, EditWikiPageInput, WikiPage, WikiPageSummary, WikiPageType,
    WikiPageVersion, WikiStore, extract_wiki_links, slugify, tolerant_replace,
};
