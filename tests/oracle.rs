//! Differential fixtures come only from the baseline's independently authored unit tests.
#[path = "support/replay.rs"]
mod replay;
use serde_json::Value;
fn normalize(mut value: Value, method: &str) -> Value {
    match method {
        "references" | "completions" => {
            if let Some(items) = value.as_array_mut() {
                items.sort_by_key(Value::to_string);
            }
        }
        "rename" => {
            if let Some(changes) = value
                .get_mut("documentChanges")
                .and_then(Value::as_array_mut)
            {
                changes.sort_by_key(|c| c["textDocument"]["uri"].to_string());
            }
        }
        _ => {}
    }
    value
}
#[test]
fn baseline_synthetic_language_operations_agree() {
    let cases: Vec<Value> = serde_json::from_str(include_str!("fixtures/oracle.json")).unwrap();
    let mut accepted_range_differences = 0;
    for (index, case) in cases.iter().enumerate() {
        let method = case["input"]["method"].as_str().unwrap();
        let result = replay::run_case(case.clone()).unwrap();
        if case.get("error").is_some() {
            assert!(
                result.get("error").is_some(),
                "case {index}: expected refusal, received {result}"
            );
            continue;
        }
        let actual = normalize(result["actual"].clone(), method);
        let expected = normalize(case["expected"].clone(), method);
        if method == "diagnostics"
            && actual != expected
            && inclusive_call_ranges(&actual, &expected)
        {
            accepted_range_differences += 1;
            continue;
        }
        assert_eq!(actual, expected, "oracle case {index}: {method}");
    }
    assert_eq!(
        accepted_range_differences, 1,
        "The intentional call-range difference should remain explicit"
    );
}
/// Native expression spans include the closing delimiter. No diagnostic is added or removed.
fn inclusive_call_ranges(actual: &Value, expected: &Value) -> bool {
    let (Some(actual), Some(expected)) = (actual.as_array(), expected.as_array()) else {
        return false;
    };
    actual.len() == expected.len()
        && actual.iter().zip(expected).all(|(a, e)| {
            if a == e {
                return true;
            }
            if a["code"] != "incompatible-reference-type"
                || a["range"]["end"]["line"] != e["range"]["end"]["line"]
            {
                return false;
            }
            let Some(end) = e["range"]["end"]["character"].as_u64() else {
                return false;
            };
            let mut adjusted = e.clone();
            adjusted["range"]["end"]["character"] = Value::from(end + 1);
            a == &adjusted
        })
}
