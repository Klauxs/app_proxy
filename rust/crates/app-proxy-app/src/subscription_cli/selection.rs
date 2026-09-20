//! Presentation only: region hints come from subscription labels, never IP lookups.
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn region(name: &str) -> &'static str {
    let name = name.to_lowercase();
    let words: Vec<_> = name.split(|c: char| !c.is_ascii_alphabetic()).collect();
    for (label, code, aliases) in [
        ("香港", "hk", &["🇭🇰", "香港", "hong kong", "hongkong"][..]),
        ("台湾", "tw", &["🇹🇼", "台湾", "台灣", "taiwan"]),
        (
            "日本",
            "jp",
            &[
                "🇯🇵", "日本", "东京", "東京", "大阪", "japan", "tokyo", "osaka",
            ],
        ),
        (
            "新加坡",
            "sg",
            &["🇸🇬", "新加坡", "狮城", "獅城", "singapore"],
        ),
        (
            "韩国",
            "kr",
            &["🇰🇷", "韩国", "韓國", "首尔", "首爾", "korea", "seoul"],
        ),
        (
            "美国",
            "us",
            &[
                "🇺🇸",
                "美国",
                "美國",
                "洛杉矶",
                "硅谷",
                "united states",
                "usa",
                "los angeles",
            ],
        ),
        (
            "英国",
            "uk",
            &["🇬🇧", "英国", "英國", "伦敦", "united kingdom", "london"],
        ),
        (
            "德国",
            "de",
            &["🇩🇪", "德国", "德國", "法兰克福", "germany", "frankfurt"],
        ),
        (
            "法国",
            "fr",
            &["🇫🇷", "法国", "法國", "巴黎", "france", "paris"],
        ),
        ("加拿大", "ca", &["🇨🇦", "加拿大", "canada", "toronto"]),
        (
            "澳大利亚",
            "au",
            &["🇦🇺", "澳大利亚", "澳洲", "悉尼", "australia", "sydney"],
        ),
        (
            "荷兰",
            "nl",
            &["🇳🇱", "荷兰", "荷蘭", "netherlands", "amsterdam"],
        ),
        (
            "印度尼西亚",
            "id",
            &["🇮🇩", "印度尼西亚", "印尼", "indonesia"],
        ),
        ("印度", "in", &["🇮🇳", "印度", "india", "mumbai"]),
        (
            "马来西亚",
            "my",
            &["🇲🇾", "马来西亚", "馬來西亞", "malaysia"],
        ),
        ("泰国", "th", &["🇹🇭", "泰国", "泰國", "thailand", "bangkok"]),
        ("越南", "vn", &["🇻🇳", "越南", "vietnam"]),
        (
            "土耳其",
            "tr",
            &["🇹🇷", "土耳其", "turkey", "türkiye", "istanbul"],
        ),
        ("菲律宾", "ph", &["🇵🇭", "菲律宾", "菲律賓", "philippines"]),
        (
            "俄罗斯",
            "ru",
            &["🇷🇺", "俄罗斯", "俄羅斯", "russia", "moscow"],
        ),
    ] {
        if words.contains(&code) || aliases.iter().any(|s| name.contains(s)) {
            return label;
        }
    }
    "其他 / 未识别"
}

pub(super) fn groups<'a>(names: impl Iterator<Item = &'a str>) -> Vec<(&'static str, Vec<usize>)> {
    let mut groups: BTreeMap<_, Vec<_>> = BTreeMap::new();
    for (index, name) in names.enumerate() {
        groups.entry(region(name)).or_default().push(index);
    }
    let other = groups.remove("其他 / 未识别");
    let mut result: Vec<_> = groups.into_iter().collect();
    if let Some(other) = other {
        result.push(("其他 / 未识别", other));
    }
    result
}

/// Global node numbers remain stable when the display is grouped.
pub(super) fn parse(
    input: &str,
    count: usize,
    groups: &[(&str, Vec<usize>)],
) -> Option<Vec<usize>> {
    let input = input.trim().to_lowercase();
    if input == "all" || input == "全部" {
        return (count > 0).then(|| (0..count).collect());
    }
    let number = |raw: &str| {
        raw.parse::<usize>()
            .ok()
            .filter(|n| *n > 0 && *n <= count)
            .map(|n| n - 1)
    };
    let mut selected = BTreeSet::new();
    for token in input
        .split([',', '，', ' ', '\t'])
        .filter(|s| !s.is_empty())
    {
        if let Some(group) = token.strip_prefix('g') {
            let n = group.parse::<usize>().ok()?.checked_sub(1)?;
            selected.extend(groups.get(n)?.1.iter().copied());
        } else if let Some((start, end)) = token.split_once('-') {
            let (start, end) = (number(start)?, number(end)?);
            if start > end {
                return None;
            }
            selected.extend(start..=end);
        } else {
            selected.insert(number(token)?);
        }
    }
    (!selected.is_empty()).then(|| selected.into_iter().collect())
}

pub(super) fn source_label(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .map(|s| {
            s.strip_prefix("www.")
                .unwrap_or(&s)
                .chars()
                .take(40)
                .collect()
        })
        .unwrap_or_else(|| "订阅".into())
}

pub(super) fn automatic_name<'a>(
    source: &str,
    selected: &[String],
    timestamp: &str,
    names: impl Iterator<Item = &'a str>,
) -> String {
    let regions: BTreeSet<_> = selected.iter().map(|n| region(n)).collect();
    let regions: Vec<_> = regions
        .into_iter()
        .map(|r| {
            if r == "其他 / 未识别" {
                "其他地区"
            } else {
                r
            }
        })
        .collect();
    let area = match regions.as_slice() {
        [] => "其他地区".into(),
        [one] => (*one).to_owned(),
        [a, b] => format!("{a}+{b}"),
        [a, b, ..] => format!("{a}+{b}等{}地区", regions.len()),
    };
    let base = format!("{source} · {area} · {timestamp}");
    let names: BTreeSet<_> = names.collect();
    if !names.contains(base.as_str()) {
        return base;
    }
    for n in 2.. {
        let candidate = format!("{base}（{n}）");
        if !names.contains(candidate.as_str()) {
            return candidate;
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_hints_do_not_match_country_codes_inside_words() {
        for (name, expected) in [
            ("🇭🇰 01", "香港"),
            ("TW-02", "台湾"),
            ("JP01", "日本"),
            ("Tokyo 03", "日本"),
            ("Australia Premium", "澳大利亚"),
            ("Business", "其他 / 未识别"),
            ("印度尼西亚 1", "印度尼西亚"),
            ("流量剩余 20GB", "其他 / 未识别"),
        ] {
            assert_eq!(region(name), expected, "{name}");
        }
    }

    #[test]
    fn multi_selection_supports_regions_ranges_and_deduplication() {
        let groups = groups(["HK-1", "JP-1", "HK-2", "unknown"].into_iter());
        let hk = groups.iter().position(|(name, _)| *name == "香港").unwrap() + 1;
        assert_eq!(
            parse(&format!("g{hk},2-3,1"), 4, &groups),
            Some(vec![0, 1, 2])
        );
        assert_eq!(parse("ALL", 4, &groups), Some(vec![0, 1, 2, 3]));
        for invalid in ["", "0", "5", "4-2", "1-500000000", "g0", "g9", "1,bad"] {
            assert_eq!(parse(invalid, 4, &groups), None, "{invalid}");
        }
        assert_eq!(groups.last().unwrap().0, "其他 / 未识别");
    }

    #[test]
    fn names_are_generated_without_subscription_secrets() {
        assert_eq!(
            source_label("https://sub.example.com/private-path?token=secret"),
            "sub.example.com"
        );
        let selected = vec!["台湾A - 服务器01".into(), "TW-2".into()];
        let name = automatic_name("示例订阅", &selected, "20260920-2130", [].into_iter());
        assert_eq!(name, "示例订阅 · 台湾 · 20260920-2130");
        assert_eq!(
            automatic_name(
                "示例订阅",
                &selected,
                "20260920-2130",
                [name.as_str()].into_iter()
            ),
            "示例订阅 · 台湾 · 20260920-2130（2）"
        );
        assert!(
            automatic_name(
                "示例",
                &["HK1".into(), "JP1".into()],
                "20260920-2130",
                [].into_iter()
            )
            .contains("日本+香港")
        );
    }
}
