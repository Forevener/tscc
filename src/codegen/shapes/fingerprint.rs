//! Canonical fingerprints for shapes and tuples — used by the walker
//! to dedupe anonymous literals against previously-registered shapes.

use oxc_ast::ast::{PropertyKey, TSPropertySignature};

use super::ShapeField;
use crate::error::CompileError;
use crate::types::BoundType;

pub(super) fn property_signature_key(prop: &TSPropertySignature) -> Result<String, CompileError> {
    match &prop.key {
        PropertyKey::StaticIdentifier(id) => Ok(id.name.as_str().to_string()),
        PropertyKey::StringLiteral(s) => Ok(s.value.as_str().to_string()),
        _ => Err(CompileError::unsupported(
            "computed property key in shape / interface type",
        )),
    }
}

/// Canonical fingerprint: sort `(name, mangle_token)` pairs by name, join as
/// `name1_ty1$name2_ty2$...`. Identical to the mangled suffix used in the
/// anonymous shape's synthetic class name.
///
/// Literal-typed fields (`kind: 'circle'`) extend their token with the
/// canonical literal form (`string$s_circle`) so two shapes that differ only
/// in their discriminator value get distinct fingerprints — and therefore
/// distinct synthetic class layouts. Without this, every variant of a
/// discriminated union would collapse onto the same shape.
pub(crate) fn fingerprint_of(fields: &[ShapeField]) -> String {
    let mut pairs: Vec<(&str, String)> = fields
        .iter()
        .map(|f| (f.name.as_str(), field_token(f)))
        .collect();
    pairs.sort_by(|a, b| a.0.cmp(b.0));
    pairs
        .into_iter()
        .map(|(n, t)| format!("{n}_{t}"))
        .collect::<Vec<_>>()
        .join("$")
}

fn field_token(f: &ShapeField) -> String {
    match &f.tag_value {
        None => f.ty.mangle_token(),
        Some(tv) => format!("{}${}", f.ty.mangle_token(), tv.canonical()),
    }
}

/// Positional fingerprint for tuples: element mangle tokens joined by `$`.
/// Distinct from `fingerprint_of` because token elements never contain the
/// `_` separator that object-field pairs produce (`name_ty`) — so the two
/// fingerprint namespaces cannot collide.
pub(super) fn tuple_fingerprint_of(elems: &[BoundType]) -> String {
    elems
        .iter()
        .map(|t| t.mangle_token())
        .collect::<Vec<_>>()
        .join("$")
}
