//! 预设: the model services the dialog fills in, each an API type and the
//! address the service gives for it, and for the services that list a great
//! many models, the families 刷新模型 adds. Only the form uses them: a saved
//! service keeps its type and address, not a preset, so none is stored or
//! synced, and the lists may change in any version.

use gpui_kit::SharedString;
use gpui_kit::component::searchable_list::SearchableListItem;

use crate::host::AiProtocol;
use crate::i18n::t;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    Anthropic,
    OpenAi,
    DeepSeek,
    Qwen,
    Kimi,
    Glm,
    Doubao,
    SiliconFlow,
    OpenRouter,
    Ollama,
    LmStudio,
    /// Any other address, typed in.
    Custom,
}

impl Preset {
    /// In the order the dialog offers them.
    pub const ALL: [Self; 12] = [
        Self::Anthropic,
        Self::OpenAi,
        Self::DeepSeek,
        Self::Qwen,
        Self::Kimi,
        Self::Glm,
        Self::Doubao,
        Self::SiliconFlow,
        Self::OpenRouter,
        Self::Ollama,
        Self::LmStudio,
        Self::Custom,
    ];

    /// Product names are the same in every language.
    pub fn label(self) -> SharedString {
        match self {
            Self::Anthropic => "Anthropic".into(),
            Self::OpenAi => "OpenAI".into(),
            Self::DeepSeek => "DeepSeek".into(),
            Self::Qwen => t!("assistant.preset.qwen"),
            Self::Kimi => "Kimi".into(),
            Self::Glm => t!("assistant.preset.glm"),
            Self::Doubao => t!("assistant.preset.doubao"),
            Self::SiliconFlow => t!("assistant.preset.siliconflow"),
            Self::OpenRouter => "OpenRouter".into(),
            Self::Ollama => "Ollama".into(),
            Self::LmStudio => "LM Studio".into(),
            Self::Custom => t!("assistant.preset.custom"),
        }
    }

    /// The API type and address it fills in; `None` for 自定义.
    pub fn service(self) -> Option<(AiProtocol, &'static str)> {
        let openai = |address| Some((AiProtocol::OpenAi, address));
        match self {
            Self::Anthropic => Some((AiProtocol::Anthropic, "https://api.anthropic.com")),
            // OpenAI's own API is Responses; the others are not all on it.
            Self::OpenAi => Some((AiProtocol::OpenAiResponses, "https://api.openai.com/v1")),
            Self::DeepSeek => openai("https://api.deepseek.com/v1"),
            Self::Qwen => openai("https://dashscope.aliyuncs.com/compatible-mode/v1"),
            Self::Kimi => openai("https://api.moonshot.cn/v1"),
            Self::Glm => openai("https://open.bigmodel.cn/api/paas/v4"),
            Self::Doubao => openai("https://ark.cn-beijing.volces.com/api/v3"),
            Self::SiliconFlow => openai("https://api.siliconflow.cn/v1"),
            Self::OpenRouter => openai("https://openrouter.ai/api/v1"),
            Self::Ollama => openai("http://localhost:11434/v1"),
            Self::LmStudio => openai("http://localhost:1234/v1"),
            Self::Custom => None,
        }
    }

    /// The families whose newest generation 刷新模型 adds, for the services
    /// that list a great many; `None` adds all a service lists. Prefixes
    /// without a version: a new generation shows up without a new ShellRS.
    fn common_families(self) -> Option<&'static [Family]> {
        use Generation::{Exact, Major};
        Some(match self {
            Self::OpenAi => &[Family("gpt-", Major)],
            Self::Qwen => &[Family("qwen", Exact)],
            Self::Doubao => &[
                Family("doubao-seed-", Exact),
                Family("deepseek-v", Major),
                Family("kimi-k", Exact),
                Family("glm-", Exact),
            ],
            Self::SiliconFlow => &[
                Family("deepseek-ai/deepseek-v", Major),
                Family("zai-org/glm-", Exact),
                Family("moonshotai/kimi-k", Exact),
                Family("qwen/qwen", Exact),
                Family("minimaxai/minimax-m", Major),
            ],
            Self::OpenRouter => &[
                Family("openai/gpt-", Major),
                Family("anthropic/claude-opus-", Major),
                Family("anthropic/claude-sonnet-", Major),
                Family("anthropic/claude-haiku-", Major),
                Family("deepseek/deepseek-v", Major),
                Family("google/gemini-", Major),
                Family("x-ai/grok-", Major),
                Family("qwen/qwen", Exact),
                Family("moonshotai/kimi-k", Exact),
                Family("z-ai/glm-", Exact),
                Family("minimax/minimax-m", Major),
                Family("tencent/hy", Major),
            ],
            _ => return None,
        })
    }

    /// Of the models a service lists, the ones 刷新模型 adds: each family's
    /// chat models of its newest generation, and its names with no version
    /// (`qwen-max`), each snapshot once, in the service's order. `None` when
    /// the preset adds them all. A name it cannot read is no error: it is
    /// left out, or added when it has no version; a few too many or too few
    /// do no harm.
    pub fn common_models(self, listed: &[String]) -> Option<Vec<String>> {
        let families = self.common_families()?;
        // Each name of a family, with its generation; `None`: no version.
        let found: Vec<(&String, usize, Option<Vec<u32>>)> = listed
            .iter()
            .filter_map(|id| {
                let lower = id.to_ascii_lowercase();
                if NOT_CHAT.iter().any(|word| lower.contains(word)) {
                    return None;
                }
                families.iter().enumerate().find_map(|(ix, family)| {
                    let rest = lower.strip_prefix(family.0)?;
                    Some((id, ix, version(rest).map(|version| family.1.of(version))))
                })
            })
            .collect();
        let newest = |family: usize| {
            found
                .iter()
                .filter(|(_, each, _)| *each == family)
                .filter_map(|(_, _, generation)| generation.as_ref())
                .max()
        };
        let wanted: Vec<&String> = found
            .iter()
            .filter(|(_, family, generation)| {
                generation.is_none() || generation.as_ref() == newest(*family)
            })
            .map(|(id, _, _)| *id)
            .collect();
        let listed_as = |name: &str| wanted.iter().any(|id| id.eq_ignore_ascii_case(name));
        Some(
            wanted
                .iter()
                .filter(|id| match stamped(id) {
                    None => true,
                    // A snapshot: its model's own name is listed, or a
                    // later snapshot of it.
                    Some((base, stamp)) => {
                        !listed_as(base)
                            && !wanted.iter().any(|other| {
                                stamped(other).is_some_and(|(other_base, other_stamp)| {
                                    other_base.eq_ignore_ascii_case(base)
                                        && other_stamp.len() == stamp.len()
                                        && *other_stamp > *stamp
                                })
                            })
                    }
                })
                .map(|id| (*id).clone())
                .collect(),
        )
    }

    /// The preset whose address `base_url` is, whatever the API type: a
    /// service can speak more than one. Else 自定义.
    pub fn of(base_url: &str) -> Self {
        let wanted = comparable(base_url);
        Self::ALL
            .into_iter()
            .find(|preset| {
                preset
                    .service()
                    .is_some_and(|(_, address)| comparable(address) == wanted)
            })
            .unwrap_or(Self::Custom)
    }
}

/// A family of models: the start of their names, the version right after
/// it (`gpt-` 6.2-sol, `deepseek-v` 4-flash, `kimi-k` 2.6).
struct Family(&'static str, Generation);

/// Which versions count as one generation.
#[derive(Clone, Copy)]
enum Generation {
    /// The first number: 6, 6.1 and 6.2 are one (GPT, Claude, DeepSeek).
    Major,
    /// The whole version: 3.6 and 3.7 are two (Qwen, GLM, Kimi, Doubao),
    /// whose every point release is a new generation.
    Exact,
}

impl Generation {
    fn of(self, mut version: Vec<u32>) -> Vec<u32> {
        if matches!(self, Self::Major) {
            version.truncate(1);
        }
        version
    }
}

/// The version a name starts with: `6.2-sol` 6.2, `4o` 4, `2-1-pro-260915`
/// 2.1, `4-0125-preview` 4, `3-32b` 3. `None` when it starts with none
/// (`-max`, `oss-120b`).
fn version(name: &str) -> Option<Vec<u32>> {
    let lead = name.bytes().take_while(u8::is_ascii_digit).count();
    let mut version = vec![name[..lead].parse().ok()?];
    let mut rest = &name[lead..];
    // More parts: one or two digits each, after `.` or `-`, ending there.
    while let Some(after) = rest.strip_prefix('.').or_else(|| rest.strip_prefix('-')) {
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        let ends = after[digits..]
            .chars()
            .next()
            .is_none_or(|next| matches!(next, '.' | '-' | ':' | '/' | '_'));
        if !(1..=2).contains(&digits) || !ends {
            break;
        }
        version.push(after[..digits].parse().ok()?);
        rest = &after[digits..];
    }
    Some(version)
}

/// Models that are not for chat, though their names start as a family's.
const NOT_CHAT: [&str; 10] = [
    "realtime",
    "audio",
    "tts",
    "transcribe",
    "search",
    "image",
    "embed",
    "moderation",
    "rerank",
    // Offered to vetted organizations only.
    "cyber",
];

/// A snapshot's name as its family's and the stamp after it: a date
/// (`-2026-10-01`, `-260915`, `-20251101`, `-1017`) or `-latest`.
fn stamped(id: &str) -> Option<(&str, &str)> {
    if let Some(base) = id.strip_suffix("-latest") {
        return Some((base, "latest"));
    }
    let (base, last) = id.rsplit_once('-')?;
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    if digits(last) && matches!(last.len(), 4 | 6 | 8) {
        return Some((base, last));
    }
    // 2026-10-01: three parts of digits.
    let (rest, day) = (base, last);
    let (rest, month) = rest.rsplit_once('-')?;
    let (base, year) = rest.rsplit_once('-')?;
    let date = digits(year) && year.len() == 4 && digits(month) && month.len() == 2;
    (date && digits(day) && day.len() == 2).then(|| (base, &id[base.len() + 1..]))
}

/// An address as typed and as listed compare equal: case and a closing
/// slash aside.
fn comparable(address: &str) -> String {
    address.trim().trim_end_matches('/').to_ascii_lowercase()
}

impl SearchableListItem for Preset {
    type Value = Self;

    fn title(&self) -> SharedString {
        self.label()
    }

    fn value(&self) -> &Self::Value {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{Preset, stamped, version};

    #[test]
    fn every_preset_but_custom_fills_in_a_service() {
        for preset in Preset::ALL {
            assert_eq!(
                preset.service().is_none(),
                preset == Preset::Custom,
                "{preset:?}"
            );
        }
    }

    #[test]
    fn an_address_finds_its_preset() {
        for preset in Preset::ALL {
            if let Some((_, address)) = preset.service() {
                assert_eq!(Preset::of(address), preset);
                assert_eq!(
                    Preset::of(&format!(" {}/ ", address.to_uppercase())),
                    preset
                );
            }
        }
        assert_eq!(Preset::of(""), Preset::Custom);
        assert_eq!(Preset::of("https://llm.example.com/v1"), Preset::Custom);
    }

    fn listed(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    #[test]
    fn a_long_list_gives_its_newest_generation() {
        let openai = listed(&[
            "gpt-4o",
            "gpt-4.1-mini",
            "gpt-5.5",
            "gpt-6.2-sol",
            "gpt-6.2-sol-2026-10-01",
            "gpt-6.2-sol-latest",
            "gpt-6-luna",
            "gpt-6-realtime",
            "gpt-6-cyber",
            "gpt-oss-120b",
            "text-embedding-3-large",
            "dall-e-3",
        ]);
        // 6 and 6.2 are one generation; a name with no version comes too.
        assert_eq!(
            Preset::OpenAi.common_models(&openai).unwrap(),
            ["gpt-6.2-sol", "gpt-6-luna", "gpt-oss-120b"]
        );
        // A generation ShellRS has not heard of.
        let mut later = openai.clone();
        later.push("gpt-7-nova".into());
        assert_eq!(
            Preset::OpenAi.common_models(&later).unwrap(),
            ["gpt-oss-120b", "gpt-7-nova"]
        );
        // Every point release a generation of its own.
        let qwen = listed(&[
            "qwen-max",
            "qwen3.6-plus",
            "qwen3.7-max",
            "qwen3.7-plus",
            "qwen3-32b",
            "qwen2.5-72b-instruct",
        ]);
        assert_eq!(
            Preset::Qwen.common_models(&qwen).unwrap(),
            ["qwen-max", "qwen3.7-max", "qwen3.7-plus"]
        );
        // Names only stamped: the latest of each.
        let doubao = listed(&[
            "doubao-seed-2-1-pro-260628",
            "doubao-seed-2-1-pro-260915",
            "doubao-seed-2-1-lite-260915",
            "doubao-seed-2-0-pro-260215",
            "doubao-seed-1-6-250615",
            "doubao-seed-evolving",
        ]);
        assert_eq!(
            Preset::Doubao.common_models(&doubao).unwrap(),
            [
                "doubao-seed-2-1-pro-260915",
                "doubao-seed-2-1-lite-260915",
                "doubao-seed-evolving"
            ]
        );
        // Case aside, and families apart.
        let siliconflow = listed(&[
            "deepseek-ai/DeepSeek-V3.2",
            "deepseek-ai/DeepSeek-V4-Pro",
            "Qwen/Qwen2.5-7B-Instruct",
            "Qwen/Qwen3.6-35B",
            "BAAI/bge-m3",
        ]);
        assert_eq!(
            Preset::SiliconFlow.common_models(&siliconflow).unwrap(),
            ["deepseek-ai/DeepSeek-V4-Pro", "Qwen/Qwen3.6-35B"]
        );
        // Nothing of the families: nothing, and no error.
        assert_eq!(
            Preset::OpenRouter
                .common_models(&listed(&["meta-llama/llama-5", "mistralai/mistral-large"]))
                .unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn versions_are_read_from_the_start_of_a_name() {
        assert_eq!(version("6.2-sol"), Some(vec![6, 2]));
        assert_eq!(version("4o"), Some(vec![4]));
        assert_eq!(version("4.1-nano"), Some(vec![4, 1]));
        assert_eq!(version("2-1-pro-260915"), Some(vec![2, 1]));
        assert_eq!(version("1-6-250615"), Some(vec![1, 6]));
        assert_eq!(version("4-0125-preview"), Some(vec![4]));
        assert_eq!(version("5-2025-08-07"), Some(vec![5]));
        assert_eq!(version("3-32b"), Some(vec![3]));
        assert_eq!(version("5-5"), Some(vec![5, 5]));
        assert_eq!(version("4-flash:free"), Some(vec![4]));
        assert_eq!(version("-max"), None);
        assert_eq!(version("oss-120b"), None);
        assert_eq!(version(""), None);
    }

    #[test]
    fn a_short_list_or_a_users_own_is_added_whole() {
        let listed = listed(&["llama4:8b", "qwen3:32b"]);
        for preset in [
            Preset::Anthropic,
            Preset::DeepSeek,
            Preset::Kimi,
            Preset::Glm,
            Preset::Ollama,
            Preset::LmStudio,
            Preset::Custom,
        ] {
            assert_eq!(preset.common_models(&listed), None, "{preset:?}");
        }
    }

    #[test]
    fn snapshots_are_told_by_their_stamps() {
        assert_eq!(stamped("gpt-6-2026-10-01"), Some(("gpt-6", "2026-10-01")));
        assert_eq!(
            stamped("doubao-seed-2-1-pro-260915"),
            Some(("doubao-seed-2-1-pro", "260915"))
        );
        assert_eq!(
            stamped("claude-opus-4-5-20251101"),
            Some(("claude-opus-4-5", "20251101"))
        );
        assert_eq!(
            stamped("deepseek-v4.1-flash-1017"),
            Some(("deepseek-v4.1-flash", "1017"))
        );
        assert_eq!(stamped("qwen-max-latest"), Some(("qwen-max", "latest")));
        for plain in [
            "gpt-6.2-sol",
            "doubao-seed-2-1",
            "glm-5",
            "kimi-k2-5",
            "qwen3-32b",
        ] {
            assert_eq!(stamped(plain), None, "{plain}");
        }
    }
}
