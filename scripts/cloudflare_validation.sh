#!/bin/env bash
set -euxo pipefail

# AI Validation Script for Cloudflare SDK
# Generates Cloudflare SDK and provides precise analysis for AI model to fix CLI code

echo "=== AI VALIDATION REPORT ==="
echo "Generated on: $(date)"
echo "Working directory: $(pwd)"
echo ""

# Build the tool first
echo "Building skygen tool..."
pushd skygen
cargo build --release
popd

# Generate SDK in crates/cloudflare directory
echo "Generating Cloudflare SDK..."
./skygen/target/release/skygen generate \
  --schema open-api-specs/cloudflare/openapiv3.json \
  --config open-api-specs/cloudflare/config.toml \
  --output-dir crates/cloudflare

echo ""
echo "=== VALIDATION RESULTS ==="
echo "SDK generated successfully in crates/cloudflare"

# Run cargo check and capture output
echo ""
echo "CARGO CHECK OUTPUT:"
echo "==================="
pushd crates/cloudflare
cargo check --quiet 2>&1
popd

# Run cargo clippy and capture output
echo ""
echo "CARGO CLIPPY OUTPUT:"
echo "==================="
pushd crates/cloudflare
cargo clippy --quiet 2>&1
popd

echo ""
echo "=== AI MODEL PROMPTING INSTRUCTIONS ==="
echo "The AI model should analyze the skygen CLI codebase and fix issues in the generation logic."
echo ""
echo "ACTUAL CODE STRUCTURE ANALYSIS:"
echo "1. Main CLI entry point: skygen/src/bin/skygen.rs"
echo "2. Core generator module: skygen/src/generator/mod.rs"
echo "3. Model generation: skygen/src/generator/model.rs"
echo "4. Operation generation: skygen/src/generator/operation.rs"
echo "5. Project generation: skygen/src/generator/project.rs"
echo "6. Client generation: skygen/src/generator/client.rs"
echo "7. Resolution logic: skygen/src/resolver/mod.rs and skygen/src/resolver/resolve.rs"
echo ""
echo "KEY GENERATION COMPONENTS TO ANALYZE:"
echo "1. Type mapping logic in model.rs"
echo "2. API operation signature generation in operation.rs"
echo "3. Template system usage in generator/"
echo "4. Reference resolution in resolver/"
echo "5. Client code generation in client.rs"
echo ""
echo "SPECIFIC ANALYSIS REQUIREMENTS:"
echo "1. EXAMINE how OpenAPI schema types map to Rust types"
echo "2. VERIFY serde attributes are correctly generated"
echo "3. CHECK model reference handling (circular refs, nested objects)"
echo "4. ANALYZE API parameter processing and validation"
echo "5. REVIEW error handling in generated code"
echo "6. CONFIRM naming conventions consistency"
echo "7. VALIDATE feature flag support in generated code"
echo ""
echo "ISSUE IDENTIFICATION FRAMEWORK:"
echo "1. Compare generated code in crates/cloudflare with Cloudflare OpenAPI spec"
echo "2. Identify where generator fails to map spec elements correctly"
echo "3. Check for missing or incorrect attributes in generated code"
echo "4. Validate that all API endpoints are properly generated"
echo "5. Ensure type mappings are accurate (String vs Option<String> vs Vec<String>)"
echo ""
echo "FIXING STRATEGY:"
echo "1. Focus on skygen/src/generator/* files for core logic fixes"
echo "2. Modify template files in skygen/src/generator/templates/ if needed"
echo "3. Update resolver logic in skygen/src/resolver/ for reference handling"
echo "4. Fix type mapping logic in model.rs and operation.rs"
echo "5. Ensure generated code compiles and passes clippy without warnings"
echo ""
echo "VALIDATION CRITERIA:"
echo "1. Generated SDK must compile with cargo check"
echo "2. Generated SDK must pass clippy with no warnings"
echo "3. Generated types must match Cloudflare API specification"
echo "4. Generated API operations must be complete and correct"
echo "5. Generated code must follow Rust best practices"
echo ""
echo "EXPECTED IMPROVEMENTS:"
echo "- Perfect mapping from Cloudflare OpenAPI spec to Rust types"
echo "- All generated code compiles and passes clippy"
echo "- Proper serde attributes on all model fields"
echo "- Correct API operation signatures and parameters"
echo "- Complete coverage of all Cloudflare API endpoints"
echo "=== VALIDATION COMPLETE ==="