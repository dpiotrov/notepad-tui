use np_core::folding::*;
use np_core::*;
fn sp(f: &[FoldRange]) -> Vec<(usize, usize)> {
    f.iter().map(|x| (x.start, x.end)).collect()
}
#[test]
fn probe() {
    // Only indentation is active for a non-brace language.
    let t2 = "fn a() {\n    /* {\n       }\n    */\n    let x = 1;\n}\n";
    println!("T2 indent-only: {:?}", sp(&compute_folds(t2, "Plain Text")));
    println!("T2 union:       {:?}", sp(&compute_folds(t2, "Rust")));
    let t1 = "fn a() {\n    let s = \"{\";\n    let t = \"}\";\n    // }\n    /* { } */\n    let u = '}';\n}\n";
    println!("T1 indent-only: {:?}", sp(&compute_folds(t1, "Plain Text")));
    println!("T1 union:       {:?}", sp(&compute_folds(t1, "Rust")));
}
