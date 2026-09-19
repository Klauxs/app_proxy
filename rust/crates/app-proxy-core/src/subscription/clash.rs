use super::{Error, INPUT_LIMIT, LINE_LIMIT, NODE_LIMIT, Parsed, Result, collect, fields};
use std::{collections::BTreeMap, sync::Arc};
use yaml_rust2::{
    parser::{Event, Parser},
    scanner::TScalarStyle,
};

const VALUE_LIMIT: usize = 131072;
const DEPTH_LIMIT: usize = 32;
#[derive(Clone)]
pub(super) struct Value {
    pub line: usize,
    pub data: Arc<Data>,
    pub depth: usize,
}
pub(super) enum Data {
    Scalar(String),
    Null,
    Seq(Vec<Value>),
    Map(BTreeMap<String, Value>),
}
impl Value {
    pub fn scalar(value: String, line: usize) -> Self {
        Self {
            line,
            data: Arc::new(Data::Scalar(value)),
            depth: 0,
        }
    }
    pub fn string(&self) -> Result<String> {
        match self.data.as_ref() {
            Data::Scalar(s) => Ok(s.clone()),
            _ => Err(Error::InvalidNode),
        }
    }
    pub fn strings(&self) -> Result<Vec<String>> {
        match self.data.as_ref() {
            Data::Scalar(s) => Ok(vec![s.clone()]),
            Data::Seq(items) if items.len() <= 32 => items.iter().map(Self::string).collect(),
            _ => Err(Error::InvalidNode),
        }
    }
    pub fn mapping(&self) -> Result<BTreeMap<String, Value>> {
        fn merge(
            value: &Value,
            budget: &mut usize,
            depth: usize,
        ) -> Result<BTreeMap<String, Value>> {
            if *budget == 0 || depth > DEPTH_LIMIT {
                return Err(Error::TooLarge);
            }
            *budget -= 1;
            let Data::Map(map) = value.data.as_ref() else {
                return Err(Error::InvalidNode);
            };
            if map.len() > 1024 || map.len() > *budget {
                return Err(Error::TooLarge);
            }
            *budget -= map.len();
            let mut result = BTreeMap::new();
            if let Some(base) = map.get("<<") {
                let bases = match base.data.as_ref() {
                    Data::Seq(items) => items.as_slice(),
                    _ => std::slice::from_ref(base),
                };
                for base in bases {
                    for (key, value) in merge(base, budget, depth + 1)? {
                        result.entry(key).or_insert(value);
                    }
                }
            }
            for (key, value) in map {
                if key != "<<" {
                    result.insert(key.clone(), value.clone());
                }
            }
            Ok(result)
        }
        merge(self, &mut 8192, 0)
    }
}
enum Pending {
    Seq(Vec<Value>),
    Map(BTreeMap<String, Value>, Option<String>),
}
struct Frame {
    line: usize,
    anchor: usize,
    body: Pending,
}
pub(super) fn parse(text: &str) -> Result<Parsed> {
    if text.len() > INPUT_LIMIT {
        return Err(Error::TooLarge);
    }
    let root = load(text)?;
    let mut root = root.mapping()?;
    let proxies = root.remove("proxies").ok_or(Error::Format)?;
    let Data::Seq(items) = proxies.data.as_ref() else {
        return Err(Error::Format);
    };
    if items.len() > NODE_LIMIT {
        return Err(Error::TooManyNodes);
    }
    collect(
        items
            .iter()
            .map(|item| (item.line, item.mapping().and_then(fields::node))),
    )
}
fn load(text: &str) -> Result<Value> {
    let mut parser = Parser::new_from_str(text);
    let mut frames: Vec<Frame> = Vec::new();
    let mut anchors: BTreeMap<usize, Value> = BTreeMap::new();
    let mut root = None;
    let mut docs = 0;
    let mut total = 0usize;
    let mut scalar_bytes = 0usize;
    loop {
        let (event, mark) = parser.next_token().map_err(|_| Error::Format)?;
        let mapping_start = matches!(event, Event::MappingStart(..));
        let plain_merge_key =
            matches!(&event, Event::Scalar(s, TScalarStyle::Plain, _, None) if s == "<<");
        total += 1;
        if total > VALUE_LIMIT {
            return Err(Error::TooLarge);
        }
        let line = mark.line();
        let (value, anchor) = match event {
            Event::StreamStart | Event::DocumentEnd => continue,
            Event::DocumentStart => {
                docs += 1;
                if docs != 1 {
                    return Err(Error::Format);
                }
                continue;
            }
            Event::StreamEnd => break,
            Event::Nothing => return Err(Error::Format),
            Event::MappingStart(anchor, tag) | Event::SequenceStart(anchor, tag) => {
                if tag.is_some() {
                    return Err(Error::UnsupportedOption);
                }
                if frames.len() >= DEPTH_LIMIT {
                    return Err(Error::TooLarge);
                }
                let body = if mapping_start {
                    Pending::Map(BTreeMap::new(), None)
                } else {
                    Pending::Seq(Vec::new())
                };
                frames.push(Frame { line, anchor, body });
                continue;
            }
            Event::MappingEnd | Event::SequenceEnd => {
                let frame = frames.pop().ok_or(Error::Format)?;
                let data = match frame.body {
                    Pending::Map(map, None) if matches!(event, Event::MappingEnd) => Data::Map(map),
                    Pending::Seq(seq) if matches!(event, Event::SequenceEnd) => Data::Seq(seq),
                    _ => return Err(Error::Format),
                };
                let depth = match &data {
                    Data::Seq(items) => items.iter().map(|v| v.depth).max().unwrap_or(0) + 1,
                    Data::Map(items) => items.values().map(|v| v.depth).max().unwrap_or(0) + 1,
                    _ => 0,
                };
                if depth > DEPTH_LIMIT {
                    return Err(Error::TooLarge);
                }
                (
                    Value {
                        line: frame.line,
                        data: Arc::new(data),
                        depth,
                    },
                    frame.anchor,
                )
            }
            Event::Scalar(value, style, anchor, tag) => {
                if tag.is_some() {
                    return Err(Error::UnsupportedOption);
                }
                scalar_bytes += value.len();
                if value.len() > LINE_LIMIT || scalar_bytes > INPUT_LIMIT {
                    return Err(Error::TooLarge);
                }
                let data = if style == TScalarStyle::Plain
                    && ["", "~", "null", "Null", "NULL"].contains(&value.as_str())
                {
                    Data::Null
                } else {
                    Data::Scalar(value)
                };
                (
                    Value {
                        line,
                        data: Arc::new(data),
                        depth: 0,
                    },
                    anchor,
                )
            }
            Event::Alias(id) => {
                let value = anchors.get(&id).ok_or(Error::Format)?;
                (
                    Value {
                        line,
                        data: value.data.clone(),
                        depth: value.depth,
                    },
                    0,
                )
            }
        };
        if anchor != 0 {
            anchors.insert(anchor, value.clone());
        }
        match frames.last_mut().map(|f| &mut f.body) {
            Some(Pending::Seq(items)) => items.push(value),
            Some(Pending::Map(map, key)) => {
                if let Some(key) = key.take() {
                    if map.insert(key, value).is_some() {
                        return Err(Error::ConflictingOptions);
                    }
                } else {
                    let name = value.string()?;
                    if name == "<<" && !plain_merge_key {
                        return Err(Error::UnsupportedOption);
                    }
                    if name.len() > 256 {
                        return Err(Error::TooLarge);
                    }
                    *key = Some(name);
                }
            }
            None => {
                if root.replace(value).is_some() {
                    return Err(Error::Format);
                }
            }
        }
    }
    if !frames.is_empty() {
        return Err(Error::Format);
    }
    root.ok_or(Error::Empty)
}
