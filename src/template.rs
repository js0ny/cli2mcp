use std::borrow::Cow;

use serde_json::Value;

/// One entry of `ToolConfig.args`, compiled at startup.
///
/// Parameter names are the 1-based placeholders (`{1}` -> `"1"`), sorted numerically and
/// deduplicated. Rendering never re-parses substituted values and never splits them.
#[derive(Debug)]
pub(crate) struct ArgumentTemplate {
    source: String,
    parameters: Vec<String>,
    segments: Vec<Segment>,
}

#[derive(Debug)]
enum Segment {
    Literal(String),
    Parameter(usize),
}

impl ArgumentTemplate {
    pub(crate) fn parse(source: String) -> Result<Self, String> {
        let bytes = source.as_bytes();
        let mut segments = Vec::new();
        let mut literal = String::new();
        let mut numbers = Vec::new();
        let mut index = 0;

        while index < bytes.len() {
            match bytes[index] {
                b'{' if bytes.get(index + 1) == Some(&b'{') => {
                    literal.push('{');
                    index += 2;
                }
                b'}' if bytes.get(index + 1) == Some(&b'}') => {
                    literal.push('}');
                    index += 2;
                }
                b'{' => {
                    let close = bytes[index + 1..]
                        .iter()
                        .position(|byte| *byte == b'}')
                        .map(|offset| index + 1 + offset)
                        .ok_or_else(|| "unclosed '{'".to_owned())?;
                    let number = parse_number(&source[index + 1..close])?;
                    if !literal.is_empty() {
                        segments.push(Segment::Literal(std::mem::take(&mut literal)));
                    }
                    segments.push(Segment::Parameter(number));
                    numbers.push(number);
                    index = close + 1;
                }
                b'}' => return Err("unmatched '}'".to_owned()),
                _ => {
                    let start = index;
                    while index < bytes.len() && !matches!(bytes[index], b'{' | b'}') {
                        index += 1;
                    }
                    literal.push_str(&source[start..index]);
                }
            }
        }
        if !literal.is_empty() {
            segments.push(Segment::Literal(literal));
        }

        numbers.sort_unstable();
        numbers.dedup();
        for segment in &mut segments {
            if let Segment::Parameter(number) = segment {
                *number = numbers
                    .binary_search(number)
                    .expect("parameter was collected during parsing");
            }
        }
        let parameters = numbers.into_iter().map(|number| number.to_string()).collect();

        Ok(Self {
            source,
            parameters,
            segments,
        })
    }

    pub(crate) fn parameters(&self) -> impl Iterator<Item = &str> {
        self.parameters.iter().map(String::as_str)
    }

    pub(crate) fn render<'a>(
        &'a self,
        arguments: &'a serde_json::Map<String, Value>,
    ) -> Result<Cow<'a, str>, String> {
        if !self
            .segments
            .iter()
            .any(|segment| matches!(segment, Segment::Parameter(_)))
        {
            return Ok(match self.segments.as_slice() {
                [] => Cow::Borrowed(""),
                [Segment::Literal(literal)] => Cow::Borrowed(literal.as_str()),
                _ => unreachable!("literal-only template holds at most one literal"),
            });
        }

        let mut rendered = String::with_capacity(self.source.len());
        for segment in &self.segments {
            match segment {
                Segment::Literal(literal) => rendered.push_str(literal),
                Segment::Parameter(index) => {
                    let name = &self.parameters[*index];
                    let value = arguments
                        .get(name)
                        .ok_or_else(|| format!("missing argument '{name}'"))?;
                    rendered.push_str(
                        value
                            .as_str()
                            .ok_or_else(|| format!("argument '{name}' must be a string"))?,
                    );
                }
            }
        }
        Ok(Cow::Owned(rendered))
    }
}

fn parse_number(digits: &str) -> Result<usize, String> {
    if digits.is_empty() {
        return Err("empty placeholder '{}'".to_owned());
    }
    if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!("'{digits}' is not a numeric placeholder"));
    }
    if digits.len() > 1 && digits.starts_with('0') {
        return Err(format!("'{digits}' must not have leading zeros"));
    }
    let number = digits
        .parse::<usize>()
        .map_err(|_| format!("placeholder '{{{digits}}}' overflows"))?;
    if number == 0 {
        return Err("placeholder numbers start at 1".to_owned());
    }
    Ok(number)
}

/// Union of the placeholders used by one tool, sorted numerically.
pub(crate) fn parameter_names(templates: &[ArgumentTemplate]) -> Vec<String> {
    let mut numbers: Vec<usize> = templates
        .iter()
        .flat_map(ArgumentTemplate::parameters)
        .filter_map(|name| name.parse().ok())
        .collect();
    numbers.sort_unstable();
    numbers.dedup();
    numbers
        .into_iter()
        .map(|number| number.to_string())
        .collect()
}

pub(crate) fn input_schema(parameters: &[String]) -> serde_json::Map<String, Value> {
    let properties = parameters
        .iter()
        .map(|name| (name.clone(), serde_json::json!({"type": "string"})))
        .collect();
    let mut schema = serde_json::Map::new();
    schema.insert("type".to_owned(), Value::from("object"));
    schema.insert("properties".to_owned(), Value::Object(properties));
    if !parameters.is_empty() {
        schema.insert("required".to_owned(), serde_json::json!(parameters));
    }
    schema.insert("additionalProperties".to_owned(), Value::Bool(false));
    schema
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn arguments(pairs: &[(&str, Value)]) -> serde_json::Map<String, Value> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), value.clone()))
            .collect()
    }

    fn rendered(template: &ArgumentTemplate, pairs: &[(&str, Value)]) -> String {
        template.render(&arguments(pairs)).unwrap().into_owned()
    }

    #[test]
    fn embedded_repeated_and_escaped_placeholders_render_single_argv_entries() {
        let template = ArgumentTemplate::parse("--file={1}".to_owned()).unwrap();
        assert_eq!(rendered(&template, &[("1", json!("my dir"))]), "--file=my dir");

        let template = ArgumentTemplate::parse("{1}/{2}".to_owned()).unwrap();
        assert_eq!(
            rendered(&template, &[("1", json!("my dir")), ("2", json!("a.txt"))]),
            "my dir/a.txt"
        );

        let template = ArgumentTemplate::parse("{1}".to_owned()).unwrap();
        assert_eq!(rendered(&template, &[("1", json!("my dir"))]), "my dir");

        let template = ArgumentTemplate::parse("{{1}}".to_owned()).unwrap();
        assert!(template.parameters().next().is_none());
        assert_eq!(rendered(&template, &[]), "{1}");

        let template = ArgumentTemplate::parse("{{{1}}}".to_owned()).unwrap();
        assert_eq!(rendered(&template, &[("1", json!("x"))]), "{x}");
    }

    #[test]
    fn parameters_are_unique_and_numerically_sorted() {
        let template = ArgumentTemplate::parse("{10}{2}{1}{2}{{3}}".to_owned()).unwrap();
        assert_eq!(
            template.parameters().collect::<Vec<_>>(),
            ["1", "2", "10"]
        );
    }

    #[test]
    fn substituted_values_are_not_reparsed() {
        let template = ArgumentTemplate::parse("{1}".to_owned()).unwrap();
        assert_eq!(
            rendered(&template, &[("1", json!("{2}"))]),
            "{2}",
        );
        assert_eq!(
            rendered(&template, &[("1", json!("a b; $(touch x) *"))]),
            "a b; $(touch x) *",
        );
    }

    #[test]
    fn literal_only_templates_borrow() {
        let template = ArgumentTemplate::parse("plain".to_owned()).unwrap();
        assert!(matches!(
            template.render(&arguments(&[])).unwrap(),
            Cow::Borrowed("plain")
        ));
    }

    #[test]
    fn invalid_placeholders_are_rejected() {
        for (source, expected) in [
            ("{0}", "placeholder numbers start at 1"),
            ("{01}", "'01' must not have leading zeros"),
            ("{}", "empty placeholder '{}'"),
            ("{name}", "'name' is not a numeric placeholder"),
            ("{1:desc}", "'1:desc' is not a numeric placeholder"),
            ("{1", "unclosed '{'"),
            ("1}", "unmatched '}'"),
            (
                "{99999999999999999999999999}",
                "placeholder '{99999999999999999999999999}' overflows",
            ),
        ] {
            assert_eq!(ArgumentTemplate::parse(source.to_owned()).unwrap_err(), expected);
        }
    }

    #[test]
    fn missing_or_non_string_arguments_are_rejected() {
        let template = ArgumentTemplate::parse("{1}{2}".to_owned()).unwrap();
        assert_eq!(
            template.render(&arguments(&[("2", json!("x"))])).unwrap_err(),
            "missing argument '1'"
        );
        assert_eq!(
            template
                .render(&arguments(&[("1", json!(1)), ("2", json!("x"))]))
                .unwrap_err(),
            "argument '1' must be a string"
        );
        assert_eq!(
            template
                .render(&arguments(&[("1", Value::Null), ("2", json!("x"))]))
                .unwrap_err(),
            "argument '1' must be a string"
        );
    }

    #[test]
    fn schema_matches_the_union_of_used_placeholders() {
        let templates = [
            ArgumentTemplate::parse("--file={1}".to_owned()).unwrap(),
            ArgumentTemplate::parse("{1}/{2}".to_owned()).unwrap(),
            ArgumentTemplate::parse("literal".to_owned()).unwrap(),
        ];
        let parameters = parameter_names(&templates);
        assert_eq!(parameters, ["1", "2"]);
        assert_eq!(
            Value::Object(input_schema(&parameters)),
            json!({
                "type": "object",
                "properties": {"1": {"type": "string"}, "2": {"type": "string"}},
                "required": ["1", "2"],
                "additionalProperties": false,
            })
        );

        assert_eq!(
            Value::Object(input_schema(&[])),
            json!({"type": "object", "properties": {}, "additionalProperties": false})
        );
    }
}
