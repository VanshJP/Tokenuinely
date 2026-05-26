//! A simple Cypher-subset parser that translates graph queries into SQL
//! over the `symbols` and `deps` tables.
//!
//! Supported syntax:
//! ```text
//! MATCH (var:Label)
//! MATCH (var:Label)-[:EDGE]->(var2:Label2)
//! MATCH (var:Label)<-[:EDGE]-(var2:Label2)
//! WHERE var.prop = 'value'
//! WHERE var.prop CONTAINS 'value'
//! RETURN var.prop, var2.prop
//! RETURN var
//! LIMIT n
//! ```

use anyhow::{bail, Result};
use rusqlite::Connection;
use serde_json::Value;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Tokens
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Token {
    // Keywords
    Match,
    Where,
    Return,
    Limit,
    Contains,
    And,
    // Punctuation
    LParen,
    RParen,
    LBracket,
    RBracket,
    Colon,
    Dot,
    Eq,
    Comma,
    Dash,
    Gt,
    Lt,
    // Literals
    Ident(String),
    Str(String),
    Num(usize),
}

fn tokenize(input: &str) -> Result<Vec<Token>> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();

    while let Some(&ch) = chars.peek() {
        match ch {
            ' ' | '\t' | '\n' | '\r' => {
                chars.next();
            }
            '(' => {
                tokens.push(Token::LParen);
                chars.next();
            }
            ')' => {
                tokens.push(Token::RParen);
                chars.next();
            }
            '[' => {
                tokens.push(Token::LBracket);
                chars.next();
            }
            ']' => {
                tokens.push(Token::RBracket);
                chars.next();
            }
            ':' => {
                tokens.push(Token::Colon);
                chars.next();
            }
            '.' => {
                tokens.push(Token::Dot);
                chars.next();
            }
            '=' => {
                tokens.push(Token::Eq);
                chars.next();
            }
            ',' => {
                tokens.push(Token::Comma);
                chars.next();
            }
            '-' => {
                tokens.push(Token::Dash);
                chars.next();
            }
            '>' => {
                tokens.push(Token::Gt);
                chars.next();
            }
            '<' => {
                tokens.push(Token::Lt);
                chars.next();
            }
            '\'' => {
                chars.next(); // opening quote
                let mut s = String::new();
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => s.push(c),
                        None => bail!("Unterminated string literal"),
                    }
                }
                tokens.push(Token::Str(s));
            }
            c if c.is_ascii_digit() => {
                let mut num_str = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_ascii_digit() {
                        num_str.push(c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                let n: usize = num_str
                    .parse()
                    .map_err(|e| anyhow::anyhow!("Invalid number '{}': {}", num_str, e))?;
                tokens.push(Token::Num(n));
            }
            c if c.is_ascii_alphanumeric() || c == '_' => {
                let mut ident = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_ascii_alphanumeric() || c == '_' {
                        ident.push(c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                let token = match ident.to_uppercase().as_str() {
                    "MATCH" => Token::Match,
                    "WHERE" => Token::Where,
                    "RETURN" => Token::Return,
                    "LIMIT" => Token::Limit,
                    "CONTAINS" => Token::Contains,
                    "AND" => Token::And,
                    _ => Token::Ident(ident),
                };
                tokens.push(token);
            }
            other => bail!("Unexpected character: '{}'", other),
        }
    }

    Ok(tokens)
}

// ---------------------------------------------------------------------------
// AST
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct CypherQuery {
    match_pattern: MatchPattern,
    where_clauses: Vec<WhereClause>,
    return_clause: ReturnClause,
    limit: Option<usize>,
}

#[derive(Debug)]
enum MatchPattern {
    SingleNode(NodePattern),
    Edge {
        left: NodePattern,
        edge_type: String,
        right: NodePattern,
        direction: EdgeDirection,
    },
}

#[derive(Debug)]
struct NodePattern {
    var: String,
    label: Option<String>,
}

#[derive(Debug)]
enum EdgeDirection {
    /// `(a)-[:E]->(b)` — left is source, right is target
    Right,
    /// `(a)<-[:E]-(b)` — right is source, left is target
    Left,
}

#[derive(Debug)]
struct WhereClause {
    var: String,
    prop: String,
    op: WhereOp,
    value: String,
}

#[derive(Debug)]
enum WhereOp {
    Equals,
    Contains,
}

#[derive(Debug)]
struct ReturnClause {
    items: Vec<ReturnItem>,
}

#[derive(Debug)]
enum ReturnItem {
    Property { var: String, prop: String },
    AllProps { var: String },
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0 }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn advance(&mut self) -> Option<Token> {
        if self.pos < self.tokens.len() {
            let tok = self.tokens[self.pos].clone();
            self.pos += 1;
            Some(tok)
        } else {
            None
        }
    }

    fn expect(&mut self, expected: &Token) -> Result<()> {
        match self.advance() {
            Some(ref t) if t == expected => Ok(()),
            Some(t) => bail!("Expected {:?}, got {:?}", expected, t),
            None => bail!("Expected {:?}, got end of input", expected),
        }
    }

    fn expect_ident(&mut self) -> Result<String> {
        match self.advance() {
            Some(Token::Ident(s)) => Ok(s),
            Some(t) => bail!("Expected identifier, got {:?}", t),
            None => bail!("Expected identifier, got end of input"),
        }
    }

    fn parse(mut self) -> Result<CypherQuery> {
        self.expect(&Token::Match)?;
        let match_pattern = self.parse_match_pattern()?;

        let mut where_clauses = Vec::new();
        while matches!(self.peek(), Some(Token::Where) | Some(Token::And)) {
            self.advance(); // consume WHERE / AND
            where_clauses.push(self.parse_where_clause()?);
        }

        self.expect(&Token::Return)?;
        let return_clause = self.parse_return_clause()?;

        let limit = if self.peek() == Some(&Token::Limit) {
            self.advance();
            match self.advance() {
                Some(Token::Num(n)) => Some(n),
                Some(t) => bail!("Expected number after LIMIT, got {:?}", t),
                None => bail!("Expected number after LIMIT"),
            }
        } else {
            None
        };

        // Ensure we consumed everything
        if self.pos < self.tokens.len() {
            bail!(
                "Unexpected tokens after query: {:?}",
                &self.tokens[self.pos..]
            );
        }

        Ok(CypherQuery {
            match_pattern,
            where_clauses,
            return_clause,
            limit,
        })
    }

    fn parse_node_pattern(&mut self) -> Result<NodePattern> {
        self.expect(&Token::LParen)?;
        let var = self.expect_ident()?;
        let label = if self.peek() == Some(&Token::Colon) {
            self.advance();
            Some(self.expect_ident()?)
        } else {
            None
        };
        self.expect(&Token::RParen)?;
        Ok(NodePattern { var, label })
    }

    fn parse_match_pattern(&mut self) -> Result<MatchPattern> {
        let left = self.parse_node_pattern()?;

        // Check for edge pattern
        match self.peek() {
            Some(Token::Dash) | Some(Token::Lt) => {
                let (edge_type, direction) = self.parse_edge()?;
                let right = self.parse_node_pattern()?;
                Ok(MatchPattern::Edge {
                    left,
                    edge_type,
                    right,
                    direction,
                })
            }
            _ => Ok(MatchPattern::SingleNode(left)),
        }
    }

    /// Parse an edge pattern: `-[:TYPE]->` or `<-[:TYPE]-`
    fn parse_edge(&mut self) -> Result<(String, EdgeDirection)> {
        if self.peek() == Some(&Token::Lt) {
            // <-[:TYPE]-
            self.advance(); // <
            self.expect(&Token::Dash)?;
            self.expect(&Token::LBracket)?;
            self.expect(&Token::Colon)?;
            let edge_type = self.expect_ident()?;
            self.expect(&Token::RBracket)?;
            self.expect(&Token::Dash)?;
            Ok((edge_type, EdgeDirection::Left))
        } else {
            // -[:TYPE]->
            self.expect(&Token::Dash)?;
            self.expect(&Token::LBracket)?;
            self.expect(&Token::Colon)?;
            let edge_type = self.expect_ident()?;
            self.expect(&Token::RBracket)?;
            self.expect(&Token::Dash)?;
            self.expect(&Token::Gt)?;
            Ok((edge_type, EdgeDirection::Right))
        }
    }

    fn parse_where_clause(&mut self) -> Result<WhereClause> {
        let var = self.expect_ident()?;
        self.expect(&Token::Dot)?;
        let prop = self.expect_ident()?;

        let op = match self.peek() {
            Some(Token::Eq) => {
                self.advance();
                WhereOp::Equals
            }
            Some(Token::Contains) => {
                self.advance();
                WhereOp::Contains
            }
            Some(t) => bail!(
                "Expected '=' or 'CONTAINS' in WHERE clause, got {:?}",
                t
            ),
            None => bail!("Unexpected end of input in WHERE clause"),
        };

        let value = match self.advance() {
            Some(Token::Str(s)) => s,
            Some(t) => bail!("Expected string literal in WHERE clause, got {:?}", t),
            None => bail!("Expected string literal in WHERE clause"),
        };

        Ok(WhereClause {
            var,
            prop,
            op,
            value,
        })
    }

    fn parse_return_clause(&mut self) -> Result<ReturnClause> {
        let mut items = Vec::new();
        loop {
            let var = self.expect_ident()?;
            if self.peek() == Some(&Token::Dot) {
                self.advance();
                let prop = self.expect_ident()?;
                items.push(ReturnItem::Property { var, prop });
            } else {
                items.push(ReturnItem::AllProps { var });
            }
            if self.peek() == Some(&Token::Comma) {
                self.advance();
            } else {
                break;
            }
        }
        Ok(ReturnClause { items })
    }
}

// ---------------------------------------------------------------------------
// SQL translation helpers
// ---------------------------------------------------------------------------

/// All symbol properties that can be projected.
const ALL_PROPS: &[&str] = &[
    "name",
    "kind",
    "path",
    "line_start",
    "line_end",
    "signature",
    "parent",
];

/// Map a Cypher label to the `symbols.kind` value (case-insensitive).
fn label_to_kind(label: &str) -> String {
    label.to_lowercase()
}

/// Validate and return the SQL column name for a Cypher property.
fn prop_to_column(prop: &str) -> Result<&str> {
    match prop {
        "name" | "kind" | "path" | "line_start" | "line_end" | "signature" | "parent" => Ok(prop),
        _ => bail!(
            "Unknown property '{}'. Supported: name, kind, path, line_start, line_end, signature, parent",
            prop
        ),
    }
}

/// Map a Cypher edge type to the `deps.kind` value.
fn edge_to_dep_kind(edge: &str) -> Result<&'static str> {
    match edge.to_uppercase().as_str() {
        "CALLS" => Ok("calls"),
        "IMPORTS" => Ok("imports"),
        _ => bail!(
            "Unknown edge type '{}'. Supported: CALLS, IMPORTS",
            edge
        ),
    }
}

/// Build the SELECT column list from the RETURN clause.
fn build_select(
    ret: &ReturnClause,
    aliases: &HashMap<String, String>,
) -> Result<String> {
    let mut parts = Vec::new();
    for item in &ret.items {
        match item {
            ReturnItem::Property { var, prop } => {
                let alias = aliases
                    .get(var)
                    .ok_or_else(|| anyhow::anyhow!("Unknown variable '{}' in RETURN", var))?;
                let col = prop_to_column(prop)?;
                parts.push(format!("{alias}.{col} AS \"{var}__{prop}\""));
            }
            ReturnItem::AllProps { var } => {
                let alias = aliases
                    .get(var)
                    .ok_or_else(|| anyhow::anyhow!("Unknown variable '{}' in RETURN", var))?;
                for &prop in ALL_PROPS {
                    parts.push(format!("{alias}.{prop} AS \"{var}__{prop}\""));
                }
            }
        }
    }
    Ok(parts.join(", "))
}

/// Translate a parsed Cypher AST into a SQL string plus bind parameters.
fn translate(query: &CypherQuery) -> Result<(String, Vec<String>)> {
    let mut params: Vec<String> = Vec::new();
    let mut aliases: HashMap<String, String> = HashMap::new();

    match &query.match_pattern {
        MatchPattern::SingleNode(node) => {
            aliases.insert(node.var.clone(), "s1".to_string());

            let select = build_select(&query.return_clause, &aliases)?;
            let select = if select.is_empty() {
                "s1.*".to_string()
            } else {
                select
            };

            let mut sql = format!("SELECT {select} FROM symbols s1");
            let mut wheres: Vec<String> = Vec::new();

            if let Some(label) = &node.label {
                wheres.push("s1.kind = ?".to_string());
                params.push(label_to_kind(label));
            }

            append_where_clauses(&query.where_clauses, &aliases, &mut wheres, &mut params)?;

            if !wheres.is_empty() {
                sql.push_str(" WHERE ");
                sql.push_str(&wheres.join(" AND "));
            }
            if let Some(limit) = query.limit {
                sql.push_str(&format!(" LIMIT {limit}"));
            }

            Ok((sql, params))
        }
        MatchPattern::Edge {
            left,
            edge_type,
            right,
            direction,
        } => {
            aliases.insert(left.var.clone(), "s1".to_string());
            aliases.insert(right.var.clone(), "s2".to_string());

            let dep_kind = edge_to_dep_kind(edge_type)?;

            let select = build_select(&query.return_clause, &aliases)?;
            let select = if select.is_empty() {
                "s1.*, s2.*".to_string()
            } else {
                select
            };

            let join = match direction {
                EdgeDirection::Right => {
                    // (s1)-[:E]->(s2): s1 is the source, s2 is the target
                    "FROM symbols s1 \
                     JOIN deps d ON d.source_path = s1.path \
                       AND (d.source_symbol IS NULL OR d.source_symbol = s1.name) \
                     JOIN symbols s2 ON d.target_symbol = s2.name \
                       AND (d.target_path IS NULL OR d.target_path = s2.path)"
                }
                EdgeDirection::Left => {
                    // (s1)<-[:E]-(s2): s2 is the source, s1 is the target
                    "FROM symbols s1 \
                     JOIN deps d ON d.target_symbol = s1.name \
                       AND (d.target_path IS NULL OR d.target_path = s1.path) \
                     JOIN symbols s2 ON d.source_path = s2.path \
                       AND (d.source_symbol IS NULL OR d.source_symbol = s2.name)"
                }
            };

            let mut sql = format!("SELECT {select} {join}");
            let mut wheres: Vec<String> = Vec::new();

            wheres.push("d.kind = ?".to_string());
            params.push(dep_kind.to_string());

            if let Some(label) = &left.label {
                wheres.push("s1.kind = ?".to_string());
                params.push(label_to_kind(label));
            }
            if let Some(label) = &right.label {
                wheres.push("s2.kind = ?".to_string());
                params.push(label_to_kind(label));
            }

            append_where_clauses(&query.where_clauses, &aliases, &mut wheres, &mut params)?;

            if !wheres.is_empty() {
                sql.push_str(" WHERE ");
                sql.push_str(&wheres.join(" AND "));
            }
            if let Some(limit) = query.limit {
                sql.push_str(&format!(" LIMIT {limit}"));
            }

            Ok((sql, params))
        }
    }
}

/// Translate WHERE clauses and append to the existing lists.
fn append_where_clauses(
    clauses: &[WhereClause],
    aliases: &HashMap<String, String>,
    wheres: &mut Vec<String>,
    params: &mut Vec<String>,
) -> Result<()> {
    for wc in clauses {
        let alias = aliases
            .get(&wc.var)
            .ok_or_else(|| anyhow::anyhow!("Unknown variable '{}' in WHERE clause", wc.var))?;
        let col = prop_to_column(&wc.prop)?;
        match wc.op {
            WhereOp::Equals => {
                wheres.push(format!("{alias}.{col} = ?"));
                params.push(wc.value.clone());
            }
            WhereOp::Contains => {
                wheres.push(format!("{alias}.{col} LIKE '%' || ? || '%'"));
                params.push(wc.value.clone());
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Parse and execute a Cypher-subset query against the `symbols` and `deps` tables.
///
/// Returns results as a list of JSON objects. Each row is an object whose keys
/// are variable names from the RETURN clause, and whose values are objects
/// containing the requested properties.
///
/// # Examples
///
/// ```text
/// MATCH (f:Function) WHERE f.name = 'main' RETURN f LIMIT 5
/// MATCH (a:Function)-[:CALLS]->(b:Function) RETURN a.name, b.name LIMIT 10
/// MATCH (s:Struct) WHERE s.name CONTAINS 'Config' RETURN s.name, s.path
/// ```
pub fn execute_cypher(conn: &Connection, query: &str) -> Result<Vec<Value>> {
    let tokens = tokenize(query)?;
    if tokens.is_empty() {
        bail!("Empty query");
    }

    let parser = Parser::new(tokens);
    let ast = parser.parse()?;
    let (sql, params) = translate(&ast)?;

    tracing::debug!(sql = %sql, params = ?params, "Cypher → SQL");

    let mut stmt = conn.prepare(&sql)?;
    let column_names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();

    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params
        .iter()
        .map(|s| s as &dyn rusqlite::types::ToSql)
        .collect();

    let rows = stmt.query_map(param_refs.as_slice(), |row| {
        let mut obj = serde_json::Map::new();
        for (i, col_name) in column_names.iter().enumerate() {
            let raw: rusqlite::types::Value = row.get(i)?;
            let json_val = match raw {
                rusqlite::types::Value::Null => Value::Null,
                rusqlite::types::Value::Integer(n) => Value::Number(n.into()),
                rusqlite::types::Value::Real(f) => serde_json::Number::from_f64(f)
                    .map(Value::Number)
                    .unwrap_or(Value::Null),
                rusqlite::types::Value::Text(s) => Value::String(s),
                rusqlite::types::Value::Blob(_) => Value::String("<blob>".to_string()),
            };

            // Column aliases use "var__prop" format — group into nested objects
            if let Some((var, prop)) = col_name.split_once("__") {
                if let Some(Value::Object(inner)) = obj.get_mut(var) {
                    inner.insert(prop.to_string(), json_val);
                } else {
                    let mut inner = serde_json::Map::new();
                    inner.insert(prop.to_string(), json_val);
                    obj.insert(var.to_string(), Value::Object(inner));
                }
            } else {
                obj.insert(col_name.clone(), json_val);
            }
        }
        Ok(Value::Object(obj))
    })?;

    let results: Vec<Value> = rows.filter_map(|r| r.ok()).collect();
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_single_node() {
        let tokens = tokenize("MATCH (f:Function) RETURN f.name").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Match,
                Token::LParen,
                Token::Ident("f".into()),
                Token::Colon,
                Token::Ident("Function".into()),
                Token::RParen,
                Token::Return,
                Token::Ident("f".into()),
                Token::Dot,
                Token::Ident("name".into()),
            ]
        );
    }

    #[test]
    fn tokenize_edge_right() {
        let tokens = tokenize("MATCH (a:Function)-[:CALLS]->(b:Function) RETURN a.name, b.name")
            .unwrap();
        assert!(tokens.contains(&Token::Dash));
        assert!(tokens.contains(&Token::Gt));
    }

    #[test]
    fn tokenize_edge_left() {
        let tokens = tokenize("MATCH (a:Function)<-[:CALLS]-(b:Method) RETURN a").unwrap();
        assert!(tokens.contains(&Token::Lt));
    }

    #[test]
    fn tokenize_where_contains() {
        let tokens =
            tokenize("MATCH (s:Struct) WHERE s.name CONTAINS 'Config' RETURN s.name").unwrap();
        assert!(tokens.contains(&Token::Where));
        assert!(tokens.contains(&Token::Contains));
        assert!(tokens.contains(&Token::Str("Config".into())));
    }

    #[test]
    fn parse_single_node_query() {
        let tokens = tokenize("MATCH (f:Function) RETURN f.name LIMIT 5").unwrap();
        let parser = Parser::new(tokens);
        let query = parser.parse().unwrap();

        assert!(matches!(query.match_pattern, MatchPattern::SingleNode(_)));
        assert_eq!(query.limit, Some(5));
    }

    #[test]
    fn parse_edge_query_right() {
        let tokens =
            tokenize("MATCH (a:Function)-[:CALLS]->(b:Function) RETURN a.name, b.name").unwrap();
        let parser = Parser::new(tokens);
        let query = parser.parse().unwrap();

        assert!(matches!(
            query.match_pattern,
            MatchPattern::Edge {
                direction: EdgeDirection::Right,
                ..
            }
        ));
    }

    #[test]
    fn parse_edge_query_left() {
        let tokens = tokenize("MATCH (a:Struct)<-[:IMPORTS]-(b:Function) RETURN a, b").unwrap();
        let parser = Parser::new(tokens);
        let query = parser.parse().unwrap();

        assert!(matches!(
            query.match_pattern,
            MatchPattern::Edge {
                direction: EdgeDirection::Left,
                ..
            }
        ));
    }

    #[test]
    fn translate_single_node() {
        let tokens = tokenize("MATCH (f:Function) WHERE f.name = 'main' RETURN f.name").unwrap();
        let parser = Parser::new(tokens);
        let ast = parser.parse().unwrap();
        let (sql, params) = translate(&ast).unwrap();

        assert!(sql.contains("FROM symbols s1"));
        assert!(sql.contains("s1.kind = ?"));
        assert!(sql.contains("s1.name = ?"));
        assert_eq!(params, vec!["function", "main"]);
    }

    #[test]
    fn translate_edge_right() {
        let tokens =
            tokenize("MATCH (a:Function)-[:CALLS]->(b:Method) RETURN a.name, b.name LIMIT 10")
                .unwrap();
        let parser = Parser::new(tokens);
        let ast = parser.parse().unwrap();
        let (sql, params) = translate(&ast).unwrap();

        assert!(sql.contains("JOIN deps d ON d.source_path = s1.path"));
        assert!(sql.contains("JOIN symbols s2 ON d.target_symbol = s2.name"));
        assert!(sql.contains("d.kind = ?"));
        assert!(sql.contains("LIMIT 10"));
        assert_eq!(params[0], "calls");
        assert_eq!(params[1], "function");
        assert_eq!(params[2], "method");
    }

    #[test]
    fn translate_contains_where() {
        let tokens =
            tokenize("MATCH (s:Struct) WHERE s.name CONTAINS 'Config' RETURN s.name, s.path")
                .unwrap();
        let parser = Parser::new(tokens);
        let ast = parser.parse().unwrap();
        let (sql, params) = translate(&ast).unwrap();

        assert!(sql.contains("LIKE '%' || ? || '%'"));
        assert_eq!(params[1], "Config");
    }

    #[test]
    fn error_on_unknown_property() {
        let tokens =
            tokenize("MATCH (f:Function) WHERE f.bogus = 'x' RETURN f.name").unwrap();
        let parser = Parser::new(tokens);
        let ast = parser.parse().unwrap();
        let result = translate(&ast);

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Unknown property"));
    }

    #[test]
    fn error_on_unknown_edge() {
        let tokens =
            tokenize("MATCH (a:Function)-[:EXTENDS]->(b:Struct) RETURN a.name").unwrap();
        let parser = Parser::new(tokens);
        let ast = parser.parse().unwrap();
        let result = translate(&ast);

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Unknown edge type"));
    }

    #[test]
    fn error_on_unterminated_string() {
        let result = tokenize("MATCH (f:Function) WHERE f.name = 'oops");
        assert!(result.is_err());
    }

    #[test]
    fn error_on_unknown_variable() {
        let tokens =
            tokenize("MATCH (f:Function) WHERE z.name = 'x' RETURN f.name").unwrap();
        let parser = Parser::new(tokens);
        let ast = parser.parse().unwrap();
        let result = translate(&ast);

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Unknown variable"));
    }
}
