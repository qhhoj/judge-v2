//! LANG-2: the Java class detection must name the class that declares `main`.
use judge_grader::languages::{LanguageRegistry, java_main_class};

const SHIPPED: &str = include_str!("../../../languages.toml");

#[test]
fn a_package_private_class_with_a_braced_member_before_main_is_detected() {
    let src = b"import java.util.*;\nclass Solution {\n  static int twice(int x) { return 2 * x; }\n  public static void main(String[] a) { System.out.println(twice(2)); }\n}\n";
    assert_eq!(java_main_class(src).as_deref(), Some("Solution"));
    let registry = LanguageRegistry::from_toml(SHIPPED).unwrap();
    assert_eq!(registry.get("JAVA").unwrap().source_name(src), "Solution.java");
}

#[test]
fn an_inner_public_class_is_not_taken_for_the_main_class() {
    let src = b"class Main {\n  public class Node { int v; }\n  public static void main(String[] a) { System.out.println(1); }\n}\n";
    assert_eq!(java_main_class(src).as_deref(), Some("Main"));
}
