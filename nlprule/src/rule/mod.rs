//! Implementations related to single rules.

use crate::types::*;
use crate::{
    filter::{Filter, Filterable},
    tokenizer::{finalize, Tokenizer},
    utils,
};
use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub(crate) mod disambiguation;
pub(crate) mod engine;
pub(crate) mod grammar;

use engine::Engine;

pub(crate) use engine::composition::MatchGraph;

/// A disambiguation rule.
/// Changes the information associcated with one or more tokens if it matches.
/// Sourced from LanguageTool. An example of how a simple rule might look in the original XML format:
///
/// ```xml
/// <rule id="NODT_HAVE" name="no determiner + have as verb/noun ->have/verb">
///    <pattern>
///        <token>
///             <exception postag="PRP$"></exception>
///             <exception regexp="yes">the|a</exception>
///        </token>
///        <marker>
///            <token case_sensitive="yes" regexp="yes">[Hh]ave|HAVE</token>
///        </marker>
///    </pattern>
///    <disambig action="replace"><wd lemma="have" pos="VB"></wd></disambig>
/// </rule>
/// ```
#[derive(Serialize, Deserialize)]
pub struct DisambiguationRule {
    pub(crate) id: String,
    pub(crate) engine: Engine,
    pub(crate) disambiguations: disambiguation::Disambiguation,
    pub(crate) filter: Option<Filter>,
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) tests: Vec<disambiguation::DisambiguationTest>,
}

#[derive(Default)]
pub struct Changes(Vec<Vec<HashSet<(usize, usize)>>>);

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl DisambiguationRule {
    /// Get a unique identifier of this rule.
    pub fn id(&self) -> &str {
        self.id.as_str()
    }

    pub(crate) fn apply<'t>(&'t self, tokens: &[Token<'t>], tokenizer: &Tokenizer) -> Changes {
        if matches!(self.disambiguations, disambiguation::Disambiguation::Nop) {
            return Changes::default();
        }

        let refs: Vec<&Token> = tokens.iter().collect();

        let mut all_byte_spans = Vec::new();

        for graph in self.engine.get_matches(&refs, self.start, self.end) {
            if let Some(filter) = &self.filter {
                if !filter.keep(&graph, tokenizer) {
                    continue;
                }
            }

            let mut byte_spans = Vec::new();

            for group_idx in self.start..self.end {
                let group = graph.by_id(group_idx).unwrap_or_else(|| {
                    panic!("{} group must exist in graph: {}", self.id, self.start)
                });

                let group_byte_spans: HashSet<_> = group
                    .tokens(graph.tokens())
                    .iter()
                    .map(|x| x.byte_span)
                    .collect();

                byte_spans.push(group_byte_spans);
            }

            all_byte_spans.push(byte_spans);
        }

        Changes(all_byte_spans)
    }

    pub fn change<'t>(
        &'t self,
        tokens: &mut Vec<IncompleteToken<'t>>,
        tokenizer: &Tokenizer,
        changes: Changes,
    ) {
        log::info!("applying {}", self.id);

        for byte_spans in changes.0 {
            let mut groups = Vec::new();
            let mut refs = tokens.iter_mut().collect::<Vec<_>>();

            for group_byte_spans in byte_spans {
                let mut group = Vec::new();

                while let Some(i) = refs
                    .iter()
                    .position(|x| group_byte_spans.contains(&x.byte_span))
                {
                    group.push(refs.remove(i));
                }

                groups.push(group);
            }

            self.disambiguations
                .apply(groups, tokenizer.options().retain_last);
        }
    }

    /// Often there are examples associated with a rule.
    /// This method checks whether the correct action is taken in the examples.
    pub fn test(&self, tokenizer: &Tokenizer) -> bool {
        let mut passes = Vec::new();

        for (i, test) in self.tests.iter().enumerate() {
            let text = match test {
                disambiguation::DisambiguationTest::Unchanged(x) => x.as_str(),
                disambiguation::DisambiguationTest::Changed(x) => x.text.as_str(),
            };

            let tokens_before =
                tokenizer.disambiguate_up_to_id(tokenizer.tokenize(text), Some(&self.id));
            let finalized = finalize(tokens_before.clone());
            let changes = self.apply(&finalized, tokenizer);
            let mut tokens_after = tokens_before.clone();
            if !changes.is_empty() {
                self.change(&mut tokens_after, tokenizer, changes);
            }

            info!("Tokens: {:#?}", tokens_before);

            let pass = match test {
                disambiguation::DisambiguationTest::Unchanged(_) => tokens_before == tokens_after,
                disambiguation::DisambiguationTest::Changed(change) => {
                    let _before = tokens_before
                        .iter()
                        .find(|x| x.char_span == change.char_span)
                        .unwrap();

                    let after = tokens_after
                        .iter()
                        .find(|x| x.char_span == change.char_span)
                        .unwrap();

                    let unordered_tags = after
                        .word
                        .tags
                        .iter()
                        .map(|x| x.to_owned_word_data())
                        .collect::<HashSet<OwnedWordData>>();
                    // need references to compare
                    let unordered_tags: HashSet<_> = unordered_tags.iter().collect();
                    let unordered_tags_change = change
                        .after
                        .tags
                        .iter()
                        .collect::<HashSet<&OwnedWordData>>();

                    after.word.text == change.after.text.as_ref_id()
                        && unordered_tags == unordered_tags_change
                }
            };

            if !pass {
                let error_str = format!(
                    "Rule {}: Test \"{:#?}\" failed. Before: {:#?}. After: {:#?}.",
                    self.id,
                    test,
                    tokens_before.into_iter().collect::<Vec<_>>(),
                    tokens_after.into_iter().collect::<Vec<_>>(),
                );

                if tokenizer
                    .options()
                    .known_failures
                    .contains(&format!("{}:{}", self.id, i))
                {
                    warn!("{}", error_str)
                } else {
                    error!("{}", error_str)
                }
            }

            passes.push(pass);
        }

        passes.iter().all(|x| *x)
    }
}

/// A grammar rule.
/// Returns a [Suggestion][crate::types::Suggestion] for change if it matches.
/// Sourced from LanguageTool. An example of how a simple rule might look in the original XML format:
///
/// ```xml
/// <rule id="DOSNT" name="he dosn't (doesn't)">
///     <pattern>
///         <token regexp="yes">do[se]n|does|dosan|doasn|dosen</token>
///         <token regexp="yes">['’`´‘]</token>
///         <token>t</token>
///     </pattern>
///     <message>Did you mean <suggestion>doesn\2t</suggestion>?</message>
///     <example correction="doesn't">He <marker>dosn't</marker> know about it.</example>
/// </rule>
/// ```
#[derive(Serialize, Deserialize)]
pub struct Rule {
    pub(crate) id: String,
    pub(crate) engine: Engine,
    pub(crate) tests: Vec<grammar::Test>,
    pub(crate) suggesters: Vec<grammar::Synthesizer>,
    pub(crate) message: grammar::Synthesizer,
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) on: bool,
}

impl Rule {
    /// Get a unique identifier of this rule.
    pub fn id(&self) -> &str {
        self.id.as_str()
    }

    /// Get whether this rule is "turned on" i. e. whether it should be used by the rule set.
    pub fn on(&self) -> bool {
        self.on
    }

    /// Turn this rule on.
    pub fn set_on(&mut self, on: bool) {
        self.on = on;
    }

    pub(crate) fn apply(&self, tokens: &[Token], tokenizer: &Tokenizer) -> Vec<Suggestion> {
        let refs: Vec<&Token> = tokens.iter().collect();
        let mut suggestions = Vec::new();

        for graph in self.engine.get_matches(&refs, self.start, self.end) {
            let start_group = graph
                .by_id(self.start)
                .unwrap_or_else(|| panic!("{} group must exist in graph: {}", self.id, self.start));
            let end_group = graph.by_id(self.end - 1).unwrap_or_else(|| {
                panic!("{} group must exist in graph: {}", self.id, self.end - 1)
            });

            let text: Vec<String> = self
                .suggesters
                .iter()
                .filter_map(|x| x.apply(&graph, tokenizer, self.start, self.end))
                .collect();

            let start = if text
                .iter()
                .all(|x| utils::no_space_chars().chars().any(|c| x.starts_with(c)))
            {
                let first_token = graph.groups()[graph.get_index(self.start).unwrap()..]
                    .iter()
                    .find(|x| !x.tokens(graph.tokens()).is_empty())
                    .unwrap()
                    .tokens(graph.tokens())[0];

                let idx = tokens
                    .iter()
                    .position(|x| std::ptr::eq(x, first_token))
                    .unwrap_or(0);

                if idx > 0 {
                    tokens[idx - 1].char_span.1
                } else {
                    start_group.char_span.0
                }
            } else {
                start_group.char_span.0
            };
            let end = end_group.char_span.1;

            // fix e. g. "Super , dass"
            let text: Vec<String> = text
                .into_iter()
                .map(|x| utils::fix_nospace_chars(&x))
                .collect();

            if !text.is_empty() {
                suggestions.push(Suggestion {
                    message: self
                        .message
                        .apply(&graph, tokenizer, self.start, self.end)
                        .expect("Rules must have a message."),
                    source: self.id.to_string(),
                    start,
                    end,
                    text,
                });
            }
        }

        suggestions
    }

    /// Grammar rules always have at least one example associated with them.
    /// This method checks whether the correct action is taken in the examples.
    pub fn test(&self, tokenizer: &Tokenizer) -> bool {
        let mut passes = Vec::new();

        for test in self.tests.iter() {
            let tokens = finalize(tokenizer.disambiguate(tokenizer.tokenize(&test.text)));
            info!("Tokens: {:#?}", tokens);
            let suggestions = self.apply(&tokens, tokenizer);

            let pass = if suggestions.len() > 1 {
                false
            } else {
                match &test.suggestion {
                    Some(correct_suggestion) => {
                        suggestions.len() == 1 && correct_suggestion == &suggestions[0]
                    }
                    None => suggestions.is_empty(),
                }
            };

            if !pass {
                warn!(
                    "Rule {}: test \"{}\" failed. Expected: {:#?}. Found: {:#?}.",
                    self.id, test.text, test.suggestion, suggestions
                );
            }

            passes.push(pass);
        }

        passes.iter().all(|x| *x)
    }
}
