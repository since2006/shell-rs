//! What the start page shows of the promotions, and when it asks again.
//!
//! The document is read when the start page shows, at most every three hours
//! (half an hour after a failure), never on a timer: a ShellRS that never
//! shows its start page never asks. A worker thread fetches the document
//! and its pictures and decodes them; the window collects the result on a
//! short timer, as the updater does, since a worker must not wake the
//! window. Nothing is kept on disk: the promotions sit below the list, so
//! coming in a moment after the page does not move anything.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_channel::{Receiver, TryRecvError};
use gpui_kit::*;
use url::Url;

use super::model::{Document, image_format, parse};
use super::source::{Answer, HttpSource, PromotionSource};
use super::{PROMOTIONS_URL, URL_ENV};
use crate::app::CatalogIcon;

/// The largest document believed to be one.
const DOCUMENT_LIMIT: u64 = 64 * 1024;
/// The largest picture: a 1200×400 banner is far smaller.
const IMAGE_LIMIT: u64 = 512 * 1024;
/// How long what was read stands before the page asks again.
const REFRESH: Duration = Duration::from_secs(3 * 60 * 60);
/// How long after a failure before the page asks again.
const RETRY: Duration = Duration::from_secs(30 * 60);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Where the promotions come from. Production has shellrs.com
/// ([`PromotionServices::system`]); the UI tests inject a fake.
#[derive(Clone)]
pub struct PromotionServices {
    pub source: Arc<dyn PromotionSource>,
    /// The document's address, which its relative addresses resolve
    /// against.
    pub document: Url,
}

impl PromotionServices {
    /// shellrs.com, in every build, those built locally too (the user's
    /// choice). A debug build reads what `SHELLRS_PROMOTIONS_URL` names
    /// instead when it is set (the site's `npm run dev`, or a preview); a
    /// release build cannot be pointed elsewhere.
    pub fn system() -> Option<Self> {
        let address = cfg!(debug_assertions)
            .then(|| std::env::var(URL_ENV).ok())
            .flatten()
            .unwrap_or_else(|| PROMOTIONS_URL.to_string());
        Some(Self {
            source: Arc::new(HttpSource),
            document: Url::parse(&address).ok()?,
        })
    }
}

/// The document read last, with the pictures that could be decoded.
#[derive(Default)]
struct Content {
    document: Document,
    pictures: HashMap<Url, Arc<RenderImage>>,
}

/// What a fetch decided.
enum Fetched {
    Replace(Content),
    /// The site has nothing to show (an empty document, or none).
    Clear,
    /// It could not be read: what is shown stays.
    Keep,
}

pub struct Promotions {
    services: Option<PromotionServices>,
    content: Content,
    /// When the page may ask again; `None` before it first has.
    next_due: Option<Instant>,
    job: Option<Receiver<Fetched>>,
    polling: bool,
}

impl Default for Promotions {
    fn default() -> Self {
        Self::new()
    }
}

impl Promotions {
    /// Without services: nothing is ever fetched (see [`Self::set_services`]).
    pub fn new() -> Self {
        Self {
            services: None,
            content: Content::default(),
            next_due: None,
            job: None,
            polling: false,
        }
    }

    /// Read from `services` from now on, starting afresh.
    pub fn set_services(&mut self, services: Option<PromotionServices>, cx: &mut Context<Self>) {
        self.services = services;
        self.job = None;
        self.next_due = None;
        self.replace(Content::default(), cx);
    }

    /// The start page shows: read the document if it is due.
    pub fn page_shown(&mut self, cx: &mut Context<Self>) {
        let Some(services) = self.services.clone() else {
            return;
        };
        let now = cx.background_executor().now();
        if self.job.is_some() || self.next_due.is_some_and(|due| now < due) {
            return;
        }
        let renderer = cx.svg_renderer();
        let (sender, receiver) = async_channel::unbounded();
        let spawned = std::thread::Builder::new()
            .name("shellrs-promotions".into())
            .spawn(move || {
                let _ = sender.send_blocking(fetch(&services, renderer));
            });
        if spawned.is_err() {
            self.next_due = Some(now + RETRY);
            return;
        }
        self.job = Some(receiver);
        self.poll(cx);
    }

    /// Whether a read is under way.
    pub fn reading(&self) -> bool {
        self.job.is_some()
    }

    /// What the start page shows for the interface in `locale`: an item
    /// whose text, address or picture it has none of in that language is
    /// left out.
    pub fn view(&self, locale: &str) -> PromotionView {
        let banners = self
            .content
            .document
            .banners
            .iter()
            .filter_map(|banner| {
                Some(BannerView {
                    image: self
                        .content
                        .pictures
                        .get(banner.image.get(locale)?)?
                        .clone(),
                    alt: banner.alt.get(locale)?.clone().into(),
                    url: banner.url.get(locale)?.to_string().into(),
                })
            })
            .collect();
        let links = self
            .content
            .document
            .links
            .iter()
            .filter_map(|link| {
                Some(LinkView {
                    text: link.text.get(locale)?.clone().into(),
                    url: link.url.get(locale)?.to_string().into(),
                    icon: link.icon,
                    color: link.color,
                    color_dark: link.color_dark,
                })
            })
            .collect();
        PromotionView { banners, links }
    }

    fn poll(&mut self, cx: &mut Context<Self>) {
        if self.polling {
            return;
        }
        self.polling = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                let running = this.update(cx, |this, cx| {
                    this.collect(cx);
                    this.polling = this.job.is_some();
                    this.polling
                });
                if !matches!(running, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    fn collect(&mut self, cx: &mut Context<Self>) {
        let Some(job) = &self.job else {
            return;
        };
        let fetched = match job.try_recv() {
            Ok(fetched) => fetched,
            Err(TryRecvError::Empty) => return,
            // The worker went without a word.
            Err(TryRecvError::Closed) => Fetched::Keep,
        };
        self.job = None;
        let now = cx.background_executor().now();
        match fetched {
            Fetched::Replace(content) => {
                self.replace(content, cx);
                self.next_due = Some(now + REFRESH);
            }
            Fetched::Clear => {
                self.replace(Content::default(), cx);
                self.next_due = Some(now + REFRESH);
            }
            Fetched::Keep => self.next_due = Some(now + RETRY),
        }
    }

    /// Show `content`; the pictures no longer shown leave the windows'
    /// texture atlases.
    fn replace(&mut self, content: Content, cx: &mut Context<Self>) {
        let old = std::mem::replace(&mut self.content, content);
        let gone: Vec<_> = old
            .pictures
            .into_values()
            .filter(|old| {
                !self
                    .content
                    .pictures
                    .values()
                    .any(|kept| Arc::ptr_eq(kept, old))
            })
            .collect();
        if !gone.is_empty() {
            cx.defer(move |cx| {
                for picture in gone {
                    cx.drop_image(picture, None);
                }
            });
        }
        cx.notify();
    }
}

/// On the worker: the document, then the pictures of its banners. A
/// picture that cannot be fetched or decoded leaves its banner out.
fn fetch(services: &PromotionServices, renderer: SvgRenderer) -> Fetched {
    let body = match services.source.get(&services.document, DOCUMENT_LIMIT) {
        Ok(Answer::Body(body)) => body,
        Ok(Answer::NotFound) => return Fetched::Clear,
        Err(_) => return Fetched::Keep,
    };
    let Ok(document) = parse(&body, &services.document) else {
        return Fetched::Keep;
    };
    if document.is_empty() {
        return Fetched::Clear;
    }
    let mut pictures = HashMap::new();
    for url in document.image_urls() {
        let Ok(Answer::Body(bytes)) = services.source.get(&url, IMAGE_LIMIT) else {
            continue;
        };
        let Some(format) = image_format(&bytes) else {
            continue;
        };
        if let Ok(picture) = Image::from_bytes(format, bytes).to_image_data(renderer.clone()) {
            pictures.insert(url, picture);
        }
    }
    Fetched::Replace(Content { document, pictures })
}

/// The promotions as the start page draws them, in one language.
#[derive(Clone, Default)]
pub struct PromotionView {
    pub banners: Vec<BannerView>,
    pub links: Vec<LinkView>,
}

impl PromotionView {
    pub fn is_empty(&self) -> bool {
        self.banners.is_empty() && self.links.is_empty()
    }
}

#[derive(Clone)]
pub struct BannerView {
    pub image: Arc<RenderImage>,
    pub alt: SharedString,
    pub url: SharedString,
}

#[derive(Clone)]
pub struct LinkView {
    pub text: SharedString,
    pub url: SharedString,
    pub icon: Option<CatalogIcon>,
    /// `0xRRGGBB` on light themes, and on dark ones without `color_dark`.
    pub color: Option<u32>,
    pub color_dark: Option<u32>,
}

impl LinkView {
    /// The link's own color on a light or dark theme, if it has one.
    pub fn color(&self, dark: bool) -> Option<u32> {
        if dark {
            self.color_dark.or(self.color)
        } else {
            self.color
        }
    }
}
