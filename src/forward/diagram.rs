//! The picture in the forward dialog that shows which kind of forward does
//! what: where the tunnel is entered, where it comes out, and what it
//! reaches from there. The words under it say the same thing with the
//! form's own addresses.

use std::time::Duration;

use gpui_kit::base::motion::{
    Easing, IterationCount, Keyframe, Keyframes, Timing, animate_keyframes,
};
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, ThemeStyled as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::CatalogIcon;
use crate::host::ForwardKind;

/// One round of the dot: across the tunnel, on to the target, then a rest
/// before it sets out again. The rounds go on for as long as the picture is
/// up.
const FLOW_ROUND: Duration = Duration::from_millis(3600);
/// How far through a round the dot leaves the tunnel for the plain hop, and
/// how far through it arrives and the rest begins.
const TUNNEL_END: f32 = 0.4;
const TRAVEL_END: f32 = 0.8;
/// The share of a hop over which the dot fades in, and again out.
const FADE: f32 = 0.25;
/// How much of the dot shows at its fullest: a mark on the line, not a
/// second thing to look at.
const DOT_OPACITY: f32 = 0.85;

/// What stands at one of the three stops of a forward.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Machine {
    /// This computer.
    Local,
    /// The SSH server the rule goes through.
    Server,
    /// The one address the forward reaches.
    Target,
    /// Wherever each client of a dynamic forward asks to go.
    Anywhere,
}

/// One stop: the tunnel's entrance, its exit, or what it reaches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Stop {
    pub role: &'static str,
    pub machine: Machine,
    pub name: SharedString,
    /// The address at this stop, or a word on what it does.
    pub detail: SharedString,
}

/// What the picture calls this computer and the server the rule goes
/// through; [`explain`] says the same words. They name the part each plays,
/// not the saved host: its name is whatever the user typed and need not read
/// as a machine, and the field under the picture already says which host it
/// is.
const LOCAL: &str = "本机";
const SERVER: &str = "SSH 服务器";

/// The three stops of a forward, in the order a connection passes them.
///
/// Every kind reads the same way, left to right: connect at the entrance,
/// cross the SSH connection, come out at the exit, reach the target. A local
/// and a remote forward differ in which machine is the entrance and which
/// the exit; a dynamic forward differs in the target.
pub(super) fn stops(kind: ForwardKind, bind: &SharedString, target: &SharedString) -> [Stop; 3] {
    let (local, server) = ((Machine::Local, LOCAL), (Machine::Server, SERVER));
    let (entrance, exit) = match kind {
        ForwardKind::Local | ForwardKind::Dynamic => (local, server),
        ForwardKind::Remote => (server, local),
    };
    let reached = match kind {
        ForwardKind::Dynamic => Stop {
            role: "目标",
            machine: Machine::Anywhere,
            name: "任意地址".into(),
            detail: "由应用指定".into(),
        },
        ForwardKind::Local | ForwardKind::Remote => Stop {
            role: "目标",
            machine: Machine::Target,
            name: "目标服务".into(),
            detail: target.clone(),
        },
    };
    [
        Stop {
            role: "隧道入口",
            machine: entrance.0,
            name: entrance.1.into(),
            detail: match kind {
                ForwardKind::Dynamic => format!("SOCKS {bind}").into(),
                ForwardKind::Local | ForwardKind::Remote => format!("监听 {bind}").into(),
            },
        },
        Stop {
            role: "隧道出口",
            machine: exit.0,
            name: exit.1.into(),
            detail: "代为连接目标".into(),
        },
        reached,
    ]
}

/// What the rule does, as one sentence with the form's own addresses.
pub(super) fn explain(kind: ForwardKind, bind: &str, target: &str) -> String {
    match kind {
        ForwardKind::Local => {
            format!("在本机连接 {bind}，就等于从 SSH 服务器连接 {target}。")
        }
        ForwardKind::Remote => {
            format!("在 SSH 服务器上连接 {bind}，就等于从本机连接 {target}。")
        }
        ForwardKind::Dynamic => {
            format!("把应用的 SOCKS5 代理设为 {bind}，它的连接都从 SSH 服务器发出。")
        }
    }
}

/// What the kind is for, on its card: the line people choose by.
pub(super) fn purpose(kind: ForwardKind) -> &'static str {
    match kind {
        ForwardKind::Local => "在本机访问服务器那边的服务",
        ForwardKind::Remote => "让服务器那边访问本机的服务",
        ForwardKind::Dynamic => "把服务器当作 SOCKS 代理",
    }
}

/// The flag `ssh` spells the kind with, for those who know it by that.
pub(super) fn ssh_flag(kind: ForwardKind) -> &'static str {
    match kind {
        ForwardKind::Local => "-L",
        ForwardKind::Remote => "-R",
        ForwardKind::Dynamic => "-D",
    }
}

/// A situation each kind is typically the answer to.
pub(super) fn typical_use(kind: ForwardKind) -> &'static str {
    match kind {
        ForwardKind::Local => "常见用途：用本机的客户端连接只对服务器开放的数据库或内网后台。",
        ForwardKind::Remote => "常见用途：把本机正在开发的服务给服务器那边访问，或接收 Webhook。",
        ForwardKind::Dynamic => "常见用途：让浏览器经由服务器上网，不必为每个地址单独建转发。",
    }
}

/// The dot on the hop it is crossing.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Dot {
    /// How far along the hop, from 0 at its start to 1 at its end.
    along: f32,
    opacity: f32,
}

/// The dot `crossed` of the way through a hop in time. It gathers speed and
/// slows again, and fades in as it sets out and out as it arrives, so it
/// neither starts with a jerk nor pops in and out at the stops.
fn crossing(crossed: f32) -> Dot {
    let shown = (crossed / FADE).min((1. - crossed) / FADE).clamp(0., 1.);
    Dot {
        along: Easing::EaseInOut.sample(crossed),
        opacity: Easing::EaseInOut.sample(shown) * DOT_OPACITY,
    }
}

/// Where the dot is `progress` of the way through a round: on the tunnel,
/// on the plain hop after it, or on neither while it rests.
fn flow_dots(progress: f32) -> (Option<Dot>, Option<Dot>) {
    if progress < TUNNEL_END {
        (Some(crossing(progress / TUNNEL_END)), None)
    } else if progress < TRAVEL_END {
        let crossed = (progress - TUNNEL_END) / (TRAVEL_END - TUNNEL_END);
        (None, Some(crossing(crossed)))
    } else {
        (None, None)
    }
}

/// The picture of one forward. Value-like: every input is a snapshot of the
/// form, and the flow's playback is keyed by `flow`.
#[derive(IntoElement)]
pub(super) struct ForwardDiagram {
    kind: ForwardKind,
    bind: SharedString,
    target: SharedString,
    flow: u64,
}

impl ForwardDiagram {
    /// `flow` names one playback of the moving dot: the same number keeps
    /// the playback it has, a new number starts it over at the entrance.
    pub(super) fn new(
        kind: ForwardKind,
        bind: impl Into<SharedString>,
        target: impl Into<SharedString>,
        flow: u64,
    ) -> Self {
        Self {
            kind,
            bind: bind.into(),
            target: target.into(),
            flow,
        }
    }
}

impl RenderOnce for ForwardDiagram {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        // The playback is gpui-kit's: a round from 0 to 1 at an even pace,
        // over and over. What the dot does within a round is `flow_dots`.
        let round = Keyframes::try_new([Keyframe::new(0.0, 0.0_f32), Keyframe::new(1.0, 1.0_f32)])
            .expect("two frames, in order");
        let flow = animate_keyframes(
            ElementId::NamedInteger("forward-flow".into(), self.flow),
            &round,
            Timing::new(FLOW_ROUND).iterations(IterationCount::Infinite),
            window,
            cx,
        );
        // Under reduced motion the playback reports the end of a round,
        // which is the rest: no dot, and the arrows carry the direction.
        let (tunnel_dot, plain_dot) = flow_dots(flow.value);

        let [entrance, exit, reached] = stops(self.kind, &self.bind, &self.target);
        let summary = format!("{} → {} → {}", entrance.name, exit.name, reached.name);
        h_flex()
            .id("forward-diagram")
            .test_support()
            .aria_label(summary)
            .w_full()
            .items_stretch()
            .gap_2()
            .child(render_stop(entrance, cx))
            .child(render_hop(Some("SSH 隧道"), tunnel_dot, cx))
            .child(render_stop(exit, cx))
            .child(render_hop(None, plain_dot, cx))
            .child(render_stop(reached, cx))
    }
}

fn machine_icon(machine: Machine) -> Icon {
    match machine {
        Machine::Local => Icon::new(CatalogIcon::Laptop),
        Machine::Server => Icon::new(CatalogIcon::Server),
        Machine::Target => Icon::new(CatalogIcon::Target),
        Machine::Anywhere => Icon::new(IconName::Globe),
    }
}

/// One stop: its role above a card with the machine and the address. All
/// three are drawn alike: an outline in the primary colour is how the kind
/// cards above mark the chosen one, and a stop is not something to choose.
fn render_stop(stop: Stop, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    v_flex()
        .flex_1()
        .min_w_0()
        .gap_1()
        .child(
            div()
                .text_xs()
                .text_center()
                .text_color(theme.muted_foreground)
                .child(stop.role),
        )
        .child(
            v_flex()
                .flex_1()
                .items_center()
                .gap_1()
                .p_2()
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
                .bg(theme.background)
                .child(machine_icon(stop.machine))
                .child(
                    div()
                        .w_full()
                        .text_sm()
                        .text_center()
                        .font_weight(FontWeight::MEDIUM)
                        .truncate()
                        .child(stop.name),
                )
                .child(
                    div()
                        .w_full()
                        .text_xs()
                        .text_center()
                        .text_color(theme.muted_foreground)
                        .truncate()
                        .child(stop.detail),
                ),
        )
}

/// The link between two stops: a line with an arrow at its end, and the dot
/// on it while it crosses this hop. The SSH hop is labelled and drawn
/// heavier; the hop from the exit to the target is an ordinary connection.
fn render_hop(label: Option<&'static str>, dot: Option<Dot>, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let tunnel = label.is_some();
    let color = if tunnel {
        theme.primary
    } else {
        theme.muted_foreground
    };
    v_flex()
        .flex_shrink_0()
        .w_20()
        .gap_1()
        .child(
            // As tall as a stop's role line, so the line below sits at the
            // middle of the cards.
            h_flex()
                .justify_center()
                .gap_1()
                .text_xs()
                .text_color(theme.muted_foreground)
                .when_some(label, |row, label| {
                    row.child(Icon::new(CatalogIcon::Lock).xsmall())
                        .child(label)
                })
                .when(!tunnel, |row| row.child("普通连接")),
        )
        .child(
            h_flex()
                .flex_1()
                .items_center()
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .h_2()
                        .child(
                            h_flex().absolute().inset_0().items_center().child(
                                div()
                                    .w_full()
                                    .map(|line| if tunnel { line.h_0p5() } else { line.h_px() })
                                    .bg(color),
                            ),
                        )
                        .when_some(dot, |line, dot| {
                            line.child(
                                div()
                                    .id("forward-flow-dot")
                                    .test_support()
                                    .absolute()
                                    .top_0()
                                    // Short of the end, where the arrow is.
                                    .left(relative(dot.along * 0.85))
                                    .size_2()
                                    .rounded_full_style(cx)
                                    .bg(theme.primary)
                                    .opacity(dot.opacity),
                            )
                        }),
                )
                .child(Icon::new(IconName::ArrowRight).xsmall().text_color(color)),
        )
}

#[cfg(test)]
mod tests {
    use super::{
        DOT_OPACITY, ForwardKind, Machine, SharedString, TRAVEL_END, TUNNEL_END, explain,
        flow_dots, stops,
    };

    fn text(value: &str) -> SharedString {
        value.to_string().into()
    }

    #[test]
    fn a_local_forward_enters_here_and_leaves_at_the_server() {
        let [entrance, exit, reached] = stops(
            ForwardKind::Local,
            &text("127.0.0.1:8080"),
            &text("db.internal:3306"),
        );
        assert_eq!(entrance.machine, Machine::Local);
        assert_eq!(entrance.name.as_ref(), "本机");
        assert_eq!(entrance.detail.as_ref(), "监听 127.0.0.1:8080");
        assert_eq!(exit.machine, Machine::Server);
        // The part it plays, not the name of the host picked for it.
        assert_eq!(exit.name.as_ref(), "SSH 服务器");
        assert_eq!(reached.machine, Machine::Target);
        assert_eq!(reached.detail.as_ref(), "db.internal:3306");
    }

    #[test]
    fn a_remote_forward_is_the_same_picture_with_the_machines_swapped() {
        let [entrance, exit, reached] = stops(
            ForwardKind::Remote,
            &text("127.0.0.1:9000"),
            &text("localhost:3000"),
        );
        assert_eq!(entrance.machine, Machine::Server);
        assert_eq!(entrance.name.as_ref(), "SSH 服务器");
        assert_eq!(entrance.detail.as_ref(), "监听 127.0.0.1:9000");
        assert_eq!(exit.machine, Machine::Local);
        assert_eq!(reached.detail.as_ref(), "localhost:3000");
    }

    #[test]
    fn a_dynamic_forward_reaches_wherever_the_client_asks() {
        let [entrance, exit, reached] = stops(
            ForwardKind::Dynamic,
            &text("127.0.0.1:1080"),
            &text("ignored"),
        );
        assert_eq!(entrance.machine, Machine::Local);
        assert_eq!(entrance.detail.as_ref(), "SOCKS 127.0.0.1:1080");
        assert_eq!(exit.machine, Machine::Server);
        assert_eq!(reached.machine, Machine::Anywhere);
        assert_eq!(reached.detail.as_ref(), "由应用指定");
    }

    #[test]
    fn the_sentence_says_who_connects_where_for_each_kind() {
        assert_eq!(
            explain(ForwardKind::Local, "127.0.0.1:8080", "db.internal:3306"),
            "在本机连接 127.0.0.1:8080，就等于从 SSH 服务器连接 db.internal:3306。"
        );
        assert_eq!(
            explain(ForwardKind::Remote, "127.0.0.1:9000", "localhost:3000"),
            "在 SSH 服务器上连接 127.0.0.1:9000，就等于从本机连接 localhost:3000。"
        );
        assert_eq!(
            explain(ForwardKind::Dynamic, "127.0.0.1:1080", ""),
            "把应用的 SOCKS5 代理设为 127.0.0.1:1080，它的连接都从 SSH 服务器发出。"
        );
    }

    #[test]
    fn the_dot_crosses_the_tunnel_then_the_plain_hop_then_rests() {
        // It sets out unseen from the entrance.
        let (tunnel, plain) = flow_dots(0.);
        let start = tunnel.expect("a round starts on the tunnel");
        assert_eq!((start.along, start.opacity), (0., 0.));
        assert_eq!(plain, None);

        // Half way across it is half way along, and fully there.
        let (tunnel, plain) = flow_dots(TUNNEL_END / 2.);
        let middle = tunnel.expect("still on the tunnel");
        assert!((middle.along - 0.5).abs() < 0.01, "{middle:?}");
        assert!((middle.opacity - DOT_OPACITY).abs() < 0.01, "{middle:?}");
        assert_eq!(plain, None);

        // It has faded out by the time it reaches the exit, and comes up
        // again on the hop beyond it: never on both at once.
        let arriving = flow_dots(TUNNEL_END - 0.001).0.expect("about to arrive");
        assert!(
            arriving.along > 0.99 && arriving.opacity < 0.01,
            "{arriving:?}"
        );
        let (tunnel, plain) = flow_dots(TUNNEL_END);
        assert_eq!(tunnel, None);
        let leaving = plain.expect("the plain hop comes next");
        assert_eq!((leaving.along, leaving.opacity), (0., 0.));

        // Then a rest with nothing on either hop.
        assert_eq!(flow_dots(TRAVEL_END), (None, None));
        assert_eq!(flow_dots(0.99), (None, None));
    }

    #[test]
    fn the_dot_never_jumps_along_a_hop() {
        // Sampled at about a frame apart, the steps stay small and forward.
        let frames = 216;
        let mut last = 0.;
        for frame in 0..frames {
            let progress = TUNNEL_END * frame as f32 / frames as f32;
            let dot = flow_dots(progress).0.expect("on the tunnel");
            assert!(dot.along >= last, "frame {frame} goes back");
            assert!(dot.along - last < 0.02, "frame {frame} jumps");
            last = dot.along;
        }
    }
}
