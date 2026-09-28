//
// © 2026 PLOMID Technology Solutions
//
// PLOMID
// Platform for Modern Intelligence and Data
//
// Author: Sainath Sapa
// GitHub: https://github.com/sainathsapa
//
// Licensed under the Apache License, Version 2.0;
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     https://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
use plomid_sql::{InMemoryCatalog, Lexer, Parser, TokenKind};

#[test]
fn test_bit_literal_lexing() {
    // Test 1: Lexing B'101010'
    println!("Test 1: Lexing B'101010'");
    match Lexer::new("B'101010'").lex() {
        Ok(tokens) => {
            for (i, tok) in tokens.iter().enumerate() {
                println!("  Token {}: {:?} = {:?}", i, tok.kind, tok.lexeme);
            }
            assert_eq!(tokens.len(), 2); // BitLiteral + Eof
            assert_eq!(tokens[0].kind, TokenKind::BitLiteral);
            assert_eq!(tokens[0].lexeme, "101010");
        }
        Err(e) => panic!("Lex error: {e}"),
    }

    // Test 2: Parsing SELECT B'101010'
    println!("\nTest 2: Parsing SELECT B'101010'");
    let tokens = Lexer::new("SELECT B'101010'").lex().unwrap();
    let mut catalog = InMemoryCatalog::new();
    let parser = Parser::new(tokens, &mut catalog);
    match parser.parse_statements() {
        Ok(stmts) => {
            for stmt in stmts {
                println!("  Parsed: {:?}", stmt);
            }
        }
        Err(e) => panic!("Parse error: {e}"),
    }
}
