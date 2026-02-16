// Additional edge case tests for operation generation

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generator::model::*;
    use openapiv3::*;

    #[test]
    fn handles_empty_responses() {
        let mut doc = make_doc();
        let mut models = ModelRegistry::default();
        let mut item = PathItem::default();
        let mut op = Operation::default();

        // Operation with no responses defined
        op.responses = Responses::default();

        item.get = Some(op);
        doc.paths.paths.insert("/empty".into(), ReferenceOr::Item(item));

        let registry = OperationGenerator::new()
            .collect_operations(&doc, &mut models)
            .expect("collect");
        
        let op = registry.get("get_empty").expect("operation");
        assert_eq!(op.response_enum.variants.len(), 0);
    }

    #[test]
    fn handles_only_default_response() {
        let mut doc = make_doc();
        let mut models = ModelRegistry::default();
        let mut item = PathItem::default();
        let mut op = Operation::default();

        let mut response = Response::default();
        response.content.insert(
            "application/json".into(),
            MediaType {
                schema: Some(ReferenceOr::Item(Schema {
                    schema_data: Default::default(),
                    schema_kind: openapiv3::SchemaKind::Type(Type::Boolean(Default::default())),
                })),
                ..Default::default()
            },
        );
        op.responses.default = Some(ReferenceOr::Item(response));

        item.get = Some(op);
        doc.paths.paths.insert("/default-only".into(), ReferenceOr::Item(item));

        let registry = OperationGenerator::new()
            .collect_operations(&doc, &mut models)
            .expect("collect");
        
        let op = registry.get("get_default_only").expect("operation");
        assert_eq!(op.response_enum.variants.len(), 1);
        assert_eq!(op.response_enum.variants[0].name, "Error");
        assert!(op.response_enum.variants[0].is_default);
    }

    #[test]
    fn handles_multiple_success_responses() {
        let mut doc = make_doc();
        let mut models = ModelRegistry::default();
        let mut item = PathItem::default();
        let mut op = Operation::default();

        // Multiple success responses (200, 201, 202)
        let mut resp200 = Response::default();
        resp200.content.insert(
            "application/json".into(),
            MediaType {
                schema: Some(ReferenceOr::Item(Schema {
                    schema_data: Default::default(),
                    schema_kind: openapiv3::SchemaKind::Type(Type::String(Default::default())),
                })),
                ..Default::default()
            },
        );
        op.responses.responses.insert(StatusCode::Code(200), ReferenceOr::Item(resp200));

        let mut resp201 = Response::default();
        resp201.content.insert(
            "application/json".into(),
            MediaType {
                schema: Some(ReferenceOr::Item(Schema {
                    schema_data: Default::default(),
                    schema_kind: openapiv3::SchemaKind::Type(Type::Integer(Default::default())),
                })),
                ..Default::default()
            },
        );
        op.responses.responses.insert(StatusCode::Code(201), ReferenceOr::Item(resp201));

        item.get = Some(op);
        doc.paths.paths.insert("/multi-success".into(), ReferenceOr::Item(item));

        let registry = OperationGenerator::new()
            .collect_operations(&doc, &mut models)
            .expect("collect");
        
        let op = registry.get("get_multi_success").expect("operation");
        // Should consolidate all success responses into single Success variant
        assert_eq!(op.response_enum.variants.len(), 1);
        assert_eq!(op.response_enum.variants[0].name, "Success");
    }

    #[test]
    fn handles_range_status_codes() {
        let mut doc = make_doc();
        let mut models = ModelRegistry::default();
        let mut item = PathItem::default();
        let mut op = Operation::default();

        // Response with status code range
        let mut response = Response::default();
        response.content.insert(
            "application/json".into(),
            MediaType {
                schema: Some(ReferenceOr::Item(Schema {
                    schema_data: Default::default(),
                    schema_kind: openapiv3::SchemaKind::Type(Type::Boolean(Default::default())),
                })),
                ..Default::default()
            },
        );
        op.responses.responses.insert(
            StatusCode::Range(4),
            ReferenceOr::Item(response)
        );

        item.get = Some(op);
        doc.paths.paths.insert("/range".into(), ReferenceOr::Item(item));

        let registry = OperationGenerator::new()
            .collect_operations(&doc, &mut models)
            .expect("collect");
        
        let op = registry.get("get_range").expect("operation");
        assert_eq!(op.response_enum.variants.len(), 1);
        assert_eq!(op.response_enum.variants[0].name, "Error");
        assert_eq!(op.response_enum.variants[0].status_match, "400..=499");
    }

    #[test]
    fn handles_operation_with_no_responses() {
        let mut doc = make_doc();
        let mut models = ModelRegistry::default();
        let mut item = PathItem::default();
        let mut op = Operation::default();

        // Operation with completely empty responses
        op.responses = Responses {
            responses: IndexMap::new(),
            default: None,
        };

        item.get = Some(op);
        doc.paths.paths.insert("/no-responses".into(), ReferenceOr::Item(item));

        let registry = OperationGenerator::new()
            .collect_operations(&doc, &mut models)
            .expect("collect");
        
        let op = registry.get("get_no_responses").expect("operation");
        assert_eq!(op.response_enum.variants.len(), 0);
    }
}
