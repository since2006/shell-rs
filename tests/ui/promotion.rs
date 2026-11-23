//! 开始页的推广: read from the site (a fake here), shown below the list of
//! recent hosts in the interface's language, opened in the browser.

use std::collections::HashMap;

use base64::Engine as _;
use gpui_kit::ScrollDelta;
use serde_json::json;
use shellrs::promotion::{Answer, FetchError, PromotionServices, PromotionSource};
use url::Url;

use crate::support::*;

const DOCUMENT: &str = "https://shellrs.com/api/v1/promotions.json";

/// A 1×1 PNG.
fn picture() -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==")
        .unwrap()
}

/// shellrs.com as the tests have it: an answer for each address, and the
/// addresses asked for, in order. Anything else is not there.
#[derive(Default)]
struct FakeSite {
    answers: Mutex<HashMap<String, Result<Answer, FetchError>>>,
    asked: Mutex<Vec<String>>,
}

impl FakeSite {
    /// `document`, with the pictures it names at `/zh.png` and `/en.png`,
    /// and something that is no picture at `/broken.png`.
    fn serving(document: serde_json::Value) -> Arc<Self> {
        let site = Arc::new(Self::default());
        site.answer(
            DOCUMENT,
            Ok(Answer::Body(document.to_string().into_bytes())),
        );
        for name in ["zh", "en"] {
            site.answer(
                &format!("https://shellrs.com/{name}.png"),
                Ok(Answer::Body(picture())),
            );
        }
        site.answer(
            "https://shellrs.com/broken.png",
            Ok(Answer::Body(b"not a picture".to_vec())),
        );
        site
    }

    fn answer(&self, url: &str, answer: Result<Answer, FetchError>) {
        self.answers.lock().unwrap().insert(url.to_string(), answer);
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
}

impl PromotionSource for FakeSite {
    fn get(&self, url: &Url, _limit: u64) -> Result<Answer, FetchError> {
        self.asked.lock().unwrap().push(url.to_string());
        self.answers
            .lock()
            .unwrap()
            .get(url.as_str())
            .cloned()
            .unwrap_or(Ok(Answer::NotFound))
    }
}

/// A banner in both languages and one whose picture is broken; a link in
/// both languages and one for Chinese only.
fn document() -> serde_json::Value {
    json!({
        "schema": 1,
        "banners": [
            {
                "image": { "zh-CN": "/zh.png", "en": "/en.png" },
                "alt": { "zh-CN": "手机版", "en": "Mobile" },
                "url": { "zh-CN": "https://shellrs.com/zh/download", "en": "https://shellrs.com/download" }
            },
            { "image": "/broken.png", "alt": "坏图", "url": "https://shellrs.com/" }
        ],
        "links": [
            {
                "text": { "zh-CN": "下载手机版", "en": "Get the app" },
                "url": "https://shellrs.com/download",
                "icon": "smartphone",
                "color": "#2563EB"
            },
            { "text": { "zh-CN": "仅中文" }, "url": "https://shellrs.com/zh" }
        ]
    })
}

fn serve(cx: &mut TestAppContext, workspace: &Entity<Workspace>, site: &Arc<FakeSite>) {
    let services = PromotionServices {
        source: site.clone(),
        document: Url::parse(DOCUMENT).unwrap(),
    };
    cx.update(|cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.set_promotion_services(Some(services), cx)
        })
    });
}

async fn wait_for_promotions(cx: &mut TestAppContext, handle: WindowHandle<Root>) {
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.try_find(("promotion-link", 0_usize)).is_some()
    })
    .await;
}

/// Until the read under way, if any, is over.
async fn wait_for_read(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    workspace: &Entity<Workspace>,
) {
    let promotions = cx.update(|cx| workspace.read(cx).promotions().clone());
    cx.wait_for(handle.into(), Duration::from_secs(2), move |_, cx| {
        !promotions.read(cx).reading()
    })
    .await;
}

fn shown(cx: &mut TestAppContext, handle: WindowHandle<Root>) -> bool {
    in_frame(cx, handle, |window, _| {
        window.try_find("start-promotions").is_some()
    })
}

fn label(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    id: (&'static str, usize),
) -> Option<String> {
    in_frame(cx, handle, |window, _| {
        window
            .try_find(id)
            .and_then(|element| element.label().map(str::to_string))
    })
}

#[gpui_kit::test]
async fn the_start_page_shows_the_sites_promotions_in_its_language(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace_with_store(cx, HostStore::empty());
    let site = FakeSite::serving(document());
    serve(cx, &workspace, &site);
    wait_for_promotions(cx, handle).await;

    // The document first, then the pictures on the same site.
    assert_eq!(
        site.asked(),
        [
            DOCUMENT,
            "https://shellrs.com/zh.png",
            "https://shellrs.com/en.png",
            "https://shellrs.com/broken.png"
        ]
    );
    assert_eq!(
        label(cx, handle, ("promotion-banner", 0)).as_deref(),
        Some("手机版")
    );
    // Its picture is none: no banner.
    assert_eq!(label(cx, handle, ("promotion-banner", 1)), None);
    assert_eq!(
        label(cx, handle, ("promotion-link", 0)).as_deref(),
        Some("下载手机版")
    );
    assert_eq!(
        label(cx, handle, ("promotion-link", 1)).as_deref(),
        Some("仅中文")
    );
    // Two to a row, left and right; one alone in its row on the left, as
    // wide as one of a pair (the gap between two banners is 12 px).
    let bounds = |cx: &mut TestAppContext, id: ElementId| {
        in_frame(cx, handle, move |window, _| window.find(id).bounds())
    };
    let column = bounds(cx, "recent-new-host".into()).right();
    let middle = bounds(cx, "start-promotions".into()).center().x;
    let links = [
        bounds(cx, ("promotion-link", 0_usize).into()),
        bounds(cx, ("promotion-link", 1_usize).into()),
    ];
    assert!(links[0].right() < middle && links[1].left() > middle);
    assert_eq!(links[1].right(), column);
    let banner = bounds(cx, ("promotion-banner", 0_usize).into());
    let row = links[1].right() - links[0].left();
    assert_eq!(banner.left(), links[0].left());
    assert!(
        (banner.size.width - (row - px(12.)) / 2.).abs() < px(0.5),
        "{banner:?} in {row:?}"
    );

    // Clicked, in the browser.
    in_frame(cx, handle, |window, cx| {
        window.click(("promotion-banner", 0_usize), cx)
    });
    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://shellrs.com/zh/download")
    );
    in_frame(cx, handle, |window, cx| {
        window.click(("promotion-link", 0_usize), cx)
    });
    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://shellrs.com/download")
    );

    // In English: English's, and nothing for what has no English.
    let settings = cx.update(|cx| workspace.read(cx).settings().clone());
    cx.update(|cx| {
        settings.update(cx, |settings, cx| {
            settings.update(
                |settings| settings.language = InterfaceLanguage::English,
                cx,
            )
        })
    });
    cx.run_until_parked();
    assert_eq!(
        label(cx, handle, ("promotion-banner", 0)).as_deref(),
        Some("Mobile")
    );
    assert_eq!(
        label(cx, handle, ("promotion-link", 0)).as_deref(),
        Some("Get the app")
    );
    assert_eq!(label(cx, handle, ("promotion-link", 1)), None);
    assert_eq!(
        bounds(cx, ("promotion-link", 0_usize).into()),
        links[0],
        "alone now, on the left as before"
    );
}

/// Nothing to show, no area: without a site (and then nothing is asked),
/// with an empty document, or with none.
#[gpui_kit::test]
async fn without_promotions_the_start_page_has_no_area_for_them(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace_with_store(cx, HostStore::empty());
    cx.run_until_parked();
    assert!(!shown(cx, handle));

    let empty = FakeSite::serving(json!({ "schema": 1, "banners": [], "links": [] }));
    serve(cx, &workspace, &empty);
    wait_for_read(cx, handle, &workspace).await;
    assert_eq!(empty.asked(), [DOCUMENT]);
    assert!(!shown(cx, handle));

    let gone = Arc::new(FakeSite::default());
    serve(cx, &workspace, &gone);
    wait_for_read(cx, handle, &workspace).await;
    assert!(!shown(cx, handle));
    assert_eq!(gone.asked(), [DOCUMENT]);
}

/// Read again only when the start page shows and three hours have passed;
/// a document that cannot be read keeps what is shown, an empty one
/// takes it away.
#[gpui_kit::test]
async fn the_promotions_are_read_again_after_three_hours(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace_with_store(cx, HostStore::empty());
    let site = FakeSite::serving(document());
    serve(cx, &workspace, &site);
    wait_for_promotions(cx, handle).await;
    let asked = site.asked().len();
    let promotions = cx.update(|cx| workspace.read(cx).promotions().clone());
    let page_shown = |cx: &mut TestAppContext| {
        promotions.update(cx, |promotions, cx| promotions.page_shown(cx));
    };

    page_shown(cx);
    wait_for_read(cx, handle, &workspace).await;
    assert_eq!(site.asked().len(), asked, "read a moment ago");

    site.answer(DOCUMENT, Err(FetchError::Network("offline".into())));
    cx.background_executor
        .advance_clock(Duration::from_secs(3 * 60 * 60));
    page_shown(cx);
    wait_for_read(cx, handle, &workspace).await;
    assert_eq!(site.asked().len(), asked + 1);
    assert!(shown(cx, handle), "kept while the site cannot be reached");

    site.answer(
        DOCUMENT,
        Ok(Answer::Body(
            json!({ "schema": 1 }).to_string().into_bytes(),
        )),
    );
    page_shown(cx);
    wait_for_read(cx, handle, &workspace).await;
    assert_eq!(
        site.asked().len(),
        asked + 1,
        "half an hour after a failure"
    );
    cx.background_executor
        .advance_clock(Duration::from_secs(30 * 60));
    page_shown(cx);
    wait_for_read(cx, handle, &workspace).await;
    assert_eq!(site.asked().len(), asked + 2);
    assert!(!shown(cx, handle), "the site took them down");
}

/// The title and its buttons at the top and the promotions at the bottom
/// stay; only the list between them scrolls.
#[gpui_kit::test]
async fn only_the_list_of_recent_hosts_scrolls(cx: &mut TestAppContext) {
    let mut store = HostStore::empty();
    let hosts: Vec<HostId> = (1..=10)
        .map(|n| {
            let id = store.insert_unnotified(HostDraft::new(
                format!("web-{n:02}"),
                format!("10.0.1.{n}"),
                22,
                "root",
                AuthKind::Password,
                None,
            ));
            store.record_connection_unnotified(id);
            id
        })
        .collect();
    let (handle, workspace) = open_sized_workspace_with_forwards(
        cx,
        store,
        Arc::new(FakeForwardProvider::default()),
        size(px(1280.), px(600.)),
    );
    let site = FakeSite::serving(document());
    serve(cx, &workspace, &site);
    wait_for_promotions(cx, handle).await;
    // Newest first.
    let (first, last) = (hosts[9], hosts[0]);

    let bounds = |cx: &mut TestAppContext, id: ElementId| {
        in_frame(cx, handle, move |window, _| window.find(id).bounds())
    };
    let button = bounds(cx, "recent-quick-connect".into());
    let banner = bounds(cx, ("promotion-banner", 0_usize).into());
    let list = bounds(cx, "recent-host-scroll".into());
    let top_row = bounds(cx, ("recent-host", first.0).into());
    assert!(banner.top() >= list.bottom(), "{banner:?} below {list:?}");
    let link = bounds(cx, ("promotion-link", 0_usize).into());
    assert_eq!(top_row.left(), link.left(), "one column");
    let last_row_shown = |cx: &mut TestAppContext| {
        in_frame(cx, handle, |window, _| {
            window
                .try_find(("recent-host", last.0))
                .is_some_and(|row| row.visible() && row.bounds().bottom() <= list.bottom())
        })
    };
    assert!(!last_row_shown(cx), "the list is longer than its room");

    in_frame(cx, handle, |window, cx| {
        window.scroll(
            "recent-host-scroll",
            ScrollDelta::Pixels(point(px(0.), px(-600.))),
            cx,
        )
    });
    assert_eq!(bounds(cx, "recent-quick-connect".into()), button);
    assert_eq!(bounds(cx, ("promotion-banner", 0_usize).into()), banner);
    assert!(bounds(cx, ("recent-host", first.0).into()).top() < top_row.top());
    assert!(last_row_shown(cx));
}
