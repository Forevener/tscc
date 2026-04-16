use oxc_allocator::Allocator;
use oxc_ast::ast::Program;
use oxc_parser::Parser;
use oxc_span::SourceType;

use crate::error::CompileError;

pub fn parse<'a>(allocator: &'a Allocator, source: &'a str) -> Result<Program<'a>, CompileError> {
    let source_type = SourceType::ts();
    let parser_return = Parser::new(allocator, source, source_type).parse();

    if !parser_return.errors.is_empty() {
        let messages: Vec<String> = parser_return.errors.iter().map(|e| e.to_string()).collect();
        return Err(CompileError::parse(messages.join("; ")));
    }

    Ok(parser_return.program)
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxc_ast::ast::Statement;

    #[test]
    fn parse_declare_function() {
        let alloc = Allocator::default();
        let program = parse(&alloc, "declare function me_x(me: i32): f64;").unwrap();
        assert_eq!(program.body.len(), 1);
        assert!(matches!(&program.body[0], Statement::FunctionDeclaration(f) if f.declare));
    }

    #[test]
    fn parse_export_function() {
        let alloc = Allocator::default();
        let program = parse(&alloc, "export function tick(me: i32): void {}").unwrap();
        assert_eq!(program.body.len(), 1);
        assert!(matches!(
            &program.body[0],
            Statement::ExportNamedDeclaration(_)
        ));
    }

    #[test]
    fn parse_error_reports() {
        let alloc = Allocator::default();
        let result = parse(&alloc, "function {{{ broken");
        assert!(result.is_err());
    }
}
