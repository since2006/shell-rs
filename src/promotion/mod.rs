//! 开始页的推广: pictures and links from shellrs.com, at the bottom of the
//! start page, shown only when the site has some.
//!
//! The site serves a static document (`model` has its format); only the
//! User-Agent goes with the request, as with the update check, and the
//! pictures must be on the same site, so ShellRS talks to shellrs.com and
//! nothing else for them.

mod model;
mod promotions;
mod source;

pub use model::{
    Banner, Document, ICONS, Invalid, Localized, MAX_BANNERS, MAX_LINKS, TextLink, image_format,
    parse,
};
pub use promotions::{BannerView, LinkView, PromotionServices, PromotionView, Promotions};
pub use source::{Answer, FetchError, HttpSource, PromotionSource};

/// Where the promotions are. Never changes once released: old versions
/// keep reading it.
pub const PROMOTIONS_URL: &str = "https://shellrs.com/api/v1/promotions.json";

/// A debug build reads the promotions from here instead, when it is set.
const URL_ENV: &str = "SHELLRS_PROMOTIONS_URL";
