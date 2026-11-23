//! The promotions document (`/api/v1/promotions.json` on shellrs.com),
//! read into what the start page can show. Its format is fixed once
//! released: fields may be added, never renamed; a change old versions
//! cannot read gets a new path (`/api/v2/…`), not a new `schema`.
//!
//! ```json
//! {
//!   "schema": 1,
//!   "enabled": true,
//!   "banners": [{ "image": "/promotions/a.jpg", "alt": "…", "url": "https://…" }],
//!   "links": [{ "text": { "zh-CN": "…", "en": "…" }, "url": "https://…",
//!               "icon": "smartphone" }]
//! }
//! ```
//!
//! `enabled` turns them all off from the site, the items kept in the file:
//! absent or `true`, they show; anything else, they do not (a switch to
//! hide them must not fail to). A field that can be translated is a string
//! for every language, or an object keyed `zh-CN` and `en`. An item that
//! cannot be read is left out and the rest are kept; a document that
//! cannot be read is no document.

use gpui_kit::ImageFormat;
use serde_json::{Map, Value};
use url::Url;

use crate::app::CatalogIcon;

/// The banners shown at most, first in the file first, whatever their
/// languages: with the links they fit the smallest window.
pub const MAX_BANNERS: usize = 2;
/// The text links shown at most.
pub const MAX_LINKS: usize = 4;
/// The longest alt text or link text, in characters.
const MAX_TEXT: usize = 120;
const MAX_URL: usize = 2048;

/// A value given for every language at once, or per language.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Localized<T> {
    Same(T),
    ByLocale { zh_cn: Option<T>, en: Option<T> },
}

impl<T> Localized<T> {
    /// The value for the interface in `locale`: its own, else English's.
    pub fn get(&self, locale: &str) -> Option<&T> {
        match self {
            Self::Same(value) => Some(value),
            Self::ByLocale { zh_cn, en } if locale == "zh-CN" => zh_cn.as_ref().or(en.as_ref()),
            Self::ByLocale { en, .. } => en.as_ref(),
        }
    }

    fn values(&self) -> impl Iterator<Item = &T> {
        let (one, two) = match self {
            Self::Same(value) => (Some(value), None),
            Self::ByLocale { zh_cn, en } => (zh_cn.as_ref(), en.as_ref()),
        };
        one.into_iter().chain(two)
    }
}

/// A picture that opens a page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Banner {
    pub image: Localized<Url>,
    /// What the picture says, for screen readers and the tooltip.
    pub alt: Localized<String>,
    pub url: Localized<Url>,
}

/// A line of text that opens a page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextLink {
    pub text: Localized<String>,
    pub url: Localized<Url>,
    pub icon: Option<CatalogIcon>,
    /// `0xRRGGBB`, on light themes, and on dark ones without `color_dark`.
    pub color: Option<u32>,
    pub color_dark: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Document {
    pub banners: Vec<Banner>,
    pub links: Vec<TextLink>,
}

impl Document {
    pub fn is_empty(&self) -> bool {
        self.banners.is_empty() && self.links.is_empty()
    }

    /// Every picture the banners show, in any language, once each.
    pub fn image_urls(&self) -> Vec<Url> {
        let mut urls: Vec<Url> = Vec::new();
        for url in self.banners.iter().flat_map(|banner| banner.image.values()) {
            if !urls.contains(url) {
                urls.push(url.clone());
            }
        }
        urls
    }
}

/// Why a document is none.
#[derive(Debug, PartialEq, Eq)]
pub struct Invalid(pub &'static str);

/// Read `bytes`, the document fetched from `base`, which relative
/// addresses in it are resolved against.
pub fn parse(bytes: &[u8], base: &Url) -> Result<Document, Invalid> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| Invalid("not JSON"))?;
    let object = value.as_object().ok_or(Invalid("not an object"))?;
    if object.get("schema").and_then(Value::as_u64) != Some(1) {
        return Err(Invalid("not schema 1"));
    }
    if !matches!(object.get("enabled"), None | Some(Value::Bool(true))) {
        return Ok(Document::default());
    }
    Ok(Document {
        banners: items(object, "banners")?
            .iter()
            .filter_map(|item| banner(item, base))
            .take(MAX_BANNERS)
            .collect(),
        links: items(object, "links")?
            .iter()
            .filter_map(|item| link(item, base))
            .take(MAX_LINKS)
            .collect(),
    })
}

/// The format of a picture, by its first bytes: PNG, JPEG or WebP, nothing
/// else whatever its name says.
pub fn image_format(bytes: &[u8]) -> Option<ImageFormat> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(ImageFormat::Png)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(ImageFormat::Jpeg)
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(ImageFormat::Webp)
    } else {
        None
    }
}

/// The icons a link may start with, by their Lucide names; the app embeds
/// these and no others.
pub const ICONS: &[(&str, CatalogIcon)] = &[
    ("smartphone", CatalogIcon::Smartphone),
    ("tablet-smartphone", CatalogIcon::TabletSmartphone),
    ("download", CatalogIcon::Download),
    ("star", CatalogIcon::Star),
    ("gift", CatalogIcon::Gift),
    ("sparkles", CatalogIcon::Sparkles),
    ("rocket", CatalogIcon::Rocket),
    ("megaphone", CatalogIcon::Megaphone),
    ("globe", CatalogIcon::Globe),
    ("info", CatalogIcon::Info),
    ("external-link", CatalogIcon::ExternalLink),
];

/// A missing array is an empty one; anything else in its place is not a
/// document.
fn items<'a>(object: &'a Map<String, Value>, key: &str) -> Result<&'a [Value], Invalid> {
    match object.get(key) {
        None => Ok(&[]),
        Some(Value::Array(items)) => Ok(items),
        Some(_) => Err(Invalid("not an array")),
    }
}

fn banner(item: &Value, base: &Url) -> Option<Banner> {
    let item = item.as_object()?;
    Some(Banner {
        image: localized(item.get("image"), |value| {
            resolve(base, value).filter(|url| url.origin() == base.origin())
        })?,
        alt: localized(item.get("alt"), text)?,
        url: localized(item.get("url"), |value| page(base, value))?,
    })
}

fn link(item: &Value, base: &Url) -> Option<TextLink> {
    let item = item.as_object()?;
    let field = |key| item.get(key).and_then(Value::as_str);
    Some(TextLink {
        text: localized(item.get("text"), text)?,
        url: localized(item.get("url"), |value| page(base, value))?,
        icon: field("icon").and_then(|name| {
            ICONS
                .iter()
                .find(|(known, _)| *known == name)
                .map(|(_, icon)| *icon)
        }),
        color: field("color").and_then(color),
        color_dark: field("color_dark").and_then(color),
    })
}

/// A string for every language, or `{ "zh-CN": …, "en": … }`; other
/// languages are left alone. `None` when no language has a good value.
fn localized<T>(value: Option<&Value>, read: impl Fn(&str) -> Option<T>) -> Option<Localized<T>> {
    match value? {
        Value::String(value) => read(value).map(Localized::Same),
        Value::Object(values) => {
            let read = |key| values.get(key).and_then(Value::as_str).and_then(&read);
            let (zh_cn, en) = (read("zh-CN"), read("en"));
            (zh_cn.is_some() || en.is_some()).then_some(Localized::ByLocale { zh_cn, en })
        }
        _ => None,
    }
}

/// One line, its runs of white space one space, not empty and not too long.
fn text(value: &str) -> Option<String> {
    let text = value.split_whitespace().collect::<Vec<_>>().join(" ");
    (!text.is_empty() && text.chars().count() <= MAX_TEXT).then_some(text)
}

/// `value` against the document's address, without a user or password.
fn resolve(base: &Url, value: &str) -> Option<Url> {
    if value.len() > MAX_URL {
        return None;
    }
    let url = base.join(value).ok()?;
    (url.username().is_empty() && url.password().is_none()).then_some(url)
}

/// A page to open: on the web over HTTPS, or where the document is (a
/// developer's local site).
fn page(base: &Url, value: &str) -> Option<Url> {
    resolve(base, value).filter(|url| url.scheme() == "https" || url.origin() == base.origin())
}

/// `#RRGGBB`.
fn color(value: &str) -> Option<u32> {
    let hex = value.strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(hex, 16).ok()
}

#[cfg(test)]
mod tests {
    use gpui_kit::AssetSource as _;
    use serde_json::json;

    use super::*;

    fn base() -> Url {
        Url::parse("https://shellrs.com/api/v1/promotions.json").unwrap()
    }

    fn read(value: Value) -> Result<Document, Invalid> {
        parse(value.to_string().as_bytes(), &base())
    }

    fn url(value: &str) -> Url {
        Url::parse(value).unwrap()
    }

    #[test]
    fn a_full_document_reads_with_its_addresses_resolved() {
        let document = read(json!({
            "schema": 1,
            "banners": [{
                "image": { "zh-CN": "/promotions/zh.jpg", "en": "/promotions/en.jpg" },
                "alt": { "zh-CN": "手机版", "en": "Mobile" },
                "url": "https://shellrs.com/download",
                "later": "a field this version does not know"
            }],
            "links": [{
                "text": "Get  the\nmobile app",
                "url": { "zh-CN": "/zh/download", "en": "/download" },
                "icon": "smartphone",
                "color": "#2563eb",
                "color_dark": "#60A5FA"
            }]
        }))
        .unwrap();
        assert_eq!(
            document.banners,
            [Banner {
                image: Localized::ByLocale {
                    zh_cn: Some(url("https://shellrs.com/promotions/zh.jpg")),
                    en: Some(url("https://shellrs.com/promotions/en.jpg")),
                },
                alt: Localized::ByLocale {
                    zh_cn: Some("手机版".into()),
                    en: Some("Mobile".into()),
                },
                url: Localized::Same(url("https://shellrs.com/download")),
            }]
        );
        let link = &document.links[0];
        assert_eq!(link.text, Localized::Same("Get the mobile app".into()));
        assert_eq!(
            link.url.get("zh-CN"),
            Some(&url("https://shellrs.com/zh/download"))
        );
        assert_eq!(link.icon, Some(CatalogIcon::Smartphone));
        assert_eq!(
            (link.color, link.color_dark),
            (Some(0x2563EB), Some(0x60A5FA))
        );
        assert_eq!(
            document.image_urls(),
            [
                url("https://shellrs.com/promotions/zh.jpg"),
                url("https://shellrs.com/promotions/en.jpg"),
            ]
        );
    }

    #[test]
    fn a_document_it_cannot_read_is_none() {
        for value in [
            json!([]),
            json!({}),
            json!({ "schema": 2, "links": [] }),
            json!({ "schema": "1" }),
            json!({ "schema": 1, "banners": {} }),
        ] {
            assert!(read(value.clone()).is_err(), "{value}");
        }
        assert!(parse(b"<html>", &base()).is_err());
        assert!(read(json!({ "schema": 1 })).unwrap().is_empty());
    }

    /// The site turns them all off without taking them out of the file;
    /// anything but `true` is off.
    #[test]
    fn the_site_switches_them_off_with_enabled() {
        let with = |enabled: Option<Value>| {
            let mut document = json!({
                "schema": 1,
                "links": [{ "text": "Mobile", "url": "https://shellrs.com/download" }]
            });
            if let Some(enabled) = enabled {
                document["enabled"] = enabled;
            }
            read(document).unwrap()
        };
        assert_eq!(with(None).links.len(), 1);
        assert_eq!(with(Some(json!(true))).links.len(), 1);
        for off in [json!(false), json!("false"), json!(0), json!(null)] {
            assert!(with(Some(off.clone())).is_empty(), "{off}");
        }
    }

    #[test]
    fn an_item_it_cannot_read_is_left_out_and_the_rest_kept() {
        let good = json!({ "text": "Mobile", "url": "https://shellrs.com/download" });
        let document = read(json!({
            "schema": 1,
            "links": [
                "not an item",
                { "text": "", "url": "https://shellrs.com/" },
                { "text": "x".repeat(121), "url": "https://shellrs.com/" },
                { "text": "No address" },
                // Not on the web over HTTPS, nor where the document is.
                { "text": "Plain", "url": "http://example.com/" },
                { "text": "Other", "url": "ftp://shellrs.com/" },
                { "text": "Login", "url": "https://user:secret@shellrs.com/" },
                good.clone(), good.clone(), good.clone(), good.clone(), good.clone(),
            ],
            "banners": [
                // A picture from elsewhere: ShellRS talks to shellrs.com only.
                { "image": "https://example.com/a.jpg", "alt": "A", "url": "/download" },
                { "image": "/a.jpg", "alt": { "ja": "日本語" }, "url": "/download" },
                { "image": "/a.jpg", "alt": "A", "url": "/download" },
                { "image": "/b.jpg", "alt": "B", "url": "/download" },
                { "image": "/c.jpg", "alt": "C", "url": "/download" },
            ]
        }))
        .unwrap();
        assert_eq!(document.links.len(), MAX_LINKS);
        assert!(
            document
                .links
                .iter()
                .all(|link| link.text == Localized::Same("Mobile".into()))
        );
        let alts: Vec<_> = document
            .banners
            .iter()
            .map(|banner| banner.alt.get("en").unwrap().as_str())
            .collect();
        assert_eq!(alts, ["A", "B"]);
    }

    #[test]
    fn a_language_without_its_own_text_falls_back_to_english() {
        let both = Localized::ByLocale {
            zh_cn: Some("中文"),
            en: Some("English"),
        };
        assert_eq!(both.get("zh-CN"), Some(&"中文"));
        assert_eq!(both.get("en"), Some(&"English"));
        let english = Localized::ByLocale {
            zh_cn: None,
            en: Some("English"),
        };
        assert_eq!(english.get("zh-CN"), Some(&"English"));
        // Only for Chinese: nothing in English.
        let chinese = Localized::ByLocale {
            zh_cn: Some("中文"),
            en: None,
        };
        assert_eq!(chinese.get("en"), None);
        assert_eq!(Localized::Same("all").get("en"), Some(&"all"));
    }

    #[test]
    fn a_local_document_may_link_to_its_own_pages() {
        let local = Url::parse("http://localhost:4321/api/v1/promotions.json").unwrap();
        let document = parse(
            json!({ "schema": 1, "links": [
                { "text": "Here", "url": "/download" },
                { "text": "Web", "url": "https://shellrs.com/download" },
                { "text": "Elsewhere", "url": "http://example.com/" },
            ]})
            .to_string()
            .as_bytes(),
            &local,
        )
        .unwrap();
        let urls: Vec<_> = document
            .links
            .iter()
            .map(|link| link.url.get("en").unwrap().as_str())
            .collect();
        assert_eq!(
            urls,
            [
                "http://localhost:4321/download",
                "https://shellrs.com/download"
            ]
        );
    }

    #[test]
    fn colors_and_icons_it_does_not_know_are_none() {
        assert_eq!(color("#00ff7F"), Some(0x00FF7F));
        for wrong in ["00ff7f", "#abc", "#GGGGGG", "#00ff7f00", "red"] {
            assert_eq!(color(wrong), None, "{wrong}");
        }
        let document = read(json!({ "schema": 1, "links": [{
            "text": "A", "url": "/", "icon": "skull", "color": "blue"
        }]}))
        .unwrap();
        assert_eq!(document.links[0].icon, None);
        assert_eq!(document.links[0].color, None);
    }

    /// Each icon a document may name is one the app carries.
    #[test]
    fn every_icon_a_link_may_name_is_embedded() {
        for (name, icon) in ICONS {
            let path = icon.path();
            assert!(
                matches!(crate::app::AppAssets.load(&path), Ok(Some(_))),
                "{name}: {path}"
            );
        }
    }

    #[test]
    fn pictures_are_told_by_their_first_bytes() {
        assert_eq!(
            image_format(b"\x89PNG\r\n\x1a\n...."),
            Some(ImageFormat::Png)
        );
        assert_eq!(
            image_format(&[0xFF, 0xD8, 0xFF, 0xE0]),
            Some(ImageFormat::Jpeg)
        );
        assert_eq!(
            image_format(b"RIFF\0\0\0\0WEBPVP8 "),
            Some(ImageFormat::Webp)
        );
        for other in [&b"GIF89a"[..], b"<svg xmlns=", b"", b"RIFF\0\0\0\0WAVE"] {
            assert_eq!(image_format(other), None);
        }
    }
}
