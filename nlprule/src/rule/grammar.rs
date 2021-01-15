use super::engine::composition::{MatchGraph, PosMatcher};
use crate::types::*;
use crate::{
    tokenizer::Tokenizer,
    utils::{self, regex::SerializeRegex},
};
use onig::Captures;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

impl std::cmp::PartialEq for Suggestion {
    fn eq(&self, other: &Suggestion) -> bool {
        let a: HashSet<&String> = self.text.iter().collect();
        let b: HashSet<&String> = other.text.iter().collect();

        a.intersection(&b).count() > 0 && other.start == self.start && other.end == self.end
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Conversion {
    Nop,
    AllLower,
    StartLower,
    AllUpper,
    StartUpper,
}

impl Conversion {
    fn convert(&self, input: &str) -> String {
        match &self {
            Conversion::Nop => input.to_string(),
            Conversion::AllLower => input.to_lowercase(),
            Conversion::StartLower => utils::apply_to_first(input, |c| c.to_lowercase().collect()),
            Conversion::AllUpper => input.to_uppercase(),
            Conversion::StartUpper => utils::apply_to_first(input, |c| c.to_uppercase().collect()),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Test {
    pub text: String,
    pub suggestion: Option<Suggestion>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct PosReplacer {
    pub(crate) matcher: PosMatcher,
}

impl PosReplacer {
    fn apply(&self, text: &str, tokenizer: &Tokenizer) -> Option<String> {
        let mut candidates: Vec<_> = tokenizer
            .tagger()
            .get_tags(
                text,
                tokenizer.options().always_add_lower_tags,
                tokenizer.options().use_compound_split_heuristic,
            )
            .iter()
            .map(|x| {
                let group_words = tokenizer
                    .tagger()
                    .get_group_members(&x.lemma.as_ref().to_string());
                let mut data = Vec::new();
                for word in group_words {
                    if let Some(i) = tokenizer
                        .tagger()
                        .get_tags(
                            word,
                            tokenizer.options().always_add_lower_tags,
                            tokenizer.options().use_compound_split_heuristic,
                        )
                        .iter()
                        .position(|x| self.matcher.is_match(x.pos_id))
                    {
                        data.push((word.to_string(), i));
                    }
                }
                data
            })
            .rev()
            .flatten()
            .collect();
        candidates.sort_by(|(_, a), (_, b)| a.cmp(b));
        if candidates.is_empty() {
            None
        } else {
            Some(candidates.remove(0).0)
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Match {
    pub(crate) id: usize,
    pub(crate) conversion: Conversion,
    pub(crate) pos_replacer: Option<PosReplacer>,
    pub(crate) regex_replacer: Option<(SerializeRegex, String)>,
}

impl Match {
    fn apply(&self, graph: &MatchGraph, tokenizer: &Tokenizer) -> Option<String> {
        let text = graph
            .by_id(self.id)
            .unwrap_or_else(|| panic!("group must exist in graph: {}", self.id))
            .text(graph.tokens()[0].text);

        let mut text = if let Some(replacer) = &self.pos_replacer {
            replacer.apply(text, tokenizer)?
        } else {
            text.to_string()
        };

        text = if let Some((regex, replacement)) = &self.regex_replacer {
            regex.replace_all(&text, |caps: &Captures| {
                utils::dollar_replace(replacement.to_string(), caps)
            })
        } else {
            text
        };

        // TODO: maybe return a vector here and propagate accordingly
        Some(self.conversion.convert(&text))
    }

    fn has_conversion(&self) -> bool {
        !matches!(self.conversion, Conversion::Nop)
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub enum SynthesizerPart {
    Text(String),
    Match(Match),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Synthesizer {
    pub(crate) use_titlecase_adjust: bool,
    pub(crate) parts: Vec<SynthesizerPart>,
}

impl Synthesizer {
    pub fn apply(
        &self,
        graph: &MatchGraph,
        tokenizer: &Tokenizer,
        start: usize,
        _end: usize,
    ) -> Option<String> {
        let mut output = Vec::new();

        let starts_with_conversion = match &self.parts[..] {
            [SynthesizerPart::Match(m), ..] => m.has_conversion(),
            _ => false,
        };

        for part in &self.parts {
            match part {
                SynthesizerPart::Text(t) => output.push(t.clone()),
                SynthesizerPart::Match(m) => {
                    output.push(m.apply(graph, tokenizer)?);
                }
            }
        }

        let suggestion = utils::normalize_whitespace(&output.join(""));

        // if the suggestion does not start with a case conversion match, make it title case if:
        // * at sentence start
        // * the replaced text is title case
        let make_uppercase = !starts_with_conversion
            && graph.groups()[graph.get_index(start).unwrap()..]
                .iter()
                .find(|x| !x.tokens(graph.tokens()).is_empty())
                .map(|group| {
                    let first_token = group.tokens(graph.tokens())[0];
                    (self.use_titlecase_adjust
                        && first_token
                            .word
                            .text
                            .as_ref()
                            .chars()
                            .next()
                            .expect("token must have at least one char")
                            .is_uppercase())
                        || first_token.byte_span.0 == 0
                })
                .unwrap_or(false);

        if make_uppercase {
            Some(utils::apply_to_first(&suggestion, |x| {
                x.to_uppercase().collect()
            }))
        } else {
            Some(suggestion)
        }
    }
}
