use super::*;

#[test]
fn exclude_simple_file_pattern() {
    let ignore = DockerIgnore::new("*.log\n").unwrap();
    assert!(ignore.is_excluded("debug.log"));
    assert!(ignore.is_excluded("app.log"));
    assert!(!ignore.is_excluded("app.txt"));
}

#[test]
fn exclude_directory_pattern() {
    let ignore = DockerIgnore::new("node_modules\n").unwrap();
    assert!(ignore.is_excluded("node_modules"));
    assert!(ignore.is_excluded("node_modules/package.json"));
}

#[test]
fn negation_reincludes() {
    let ignore = DockerIgnore::new("*.md\n!README.md\n").unwrap();
    assert!(ignore.is_excluded("CONTRIBUTING.md"));
    assert!(!ignore.is_excluded("README.md"));
}

#[test]
fn comments_and_blank_lines_ignored() {
    let ignore = DockerIgnore::new("# this is a comment\n\n*.tmp\n").unwrap();
    assert!(ignore.is_excluded("file.tmp"));
    assert!(!ignore.is_excluded("file.txt"));
}

#[test]
fn double_star_glob() {
    let ignore = DockerIgnore::new("**/*.o\n").unwrap();
    assert!(ignore.is_excluded("build/main.o"));
    assert!(ignore.is_excluded("src/deep/nested/lib.o"));
    assert!(!ignore.is_excluded("main.c"));
}

#[test]
fn filter_multiple_paths() {
    let ignore = DockerIgnore::new("*.log\ntmp/\n").unwrap();
    let paths = vec!["src/main.rs", "debug.log", "tmp/cache", "README.md"];
    let included = ignore.filter_paths(&paths);
    assert_eq!(included, vec!["src/main.rs", "README.md"]);
}

#[test]
fn empty_dockerignore_excludes_nothing() {
    let ignore = DockerIgnore::new("").unwrap();
    assert!(!ignore.is_excluded("anything.rs"));
    assert!(!ignore.is_excluded("node_modules/deep/file"));
}

#[test]
fn dockerfile_and_dockerignore_never_excluded() {
    let ignore = DockerIgnore::new("*\n").unwrap();
    assert!(!ignore.is_excluded("Dockerfile"));
    assert!(!ignore.is_excluded(".dockerignore"));
}

#[test]
fn leading_slash_anchors_to_root() {
    let ignore = DockerIgnore::new("/build\n").unwrap();
    assert!(ignore.is_excluded("build"));
    assert!(ignore.is_excluded("build/output.o"));
    // /build should NOT match nested paths like src/build
    assert!(!ignore.is_excluded("src/build"));
}

#[test]
fn wildcard_in_directory() {
    let ignore = DockerIgnore::new("temp*\n").unwrap();
    assert!(ignore.is_excluded("temporary"));
    assert!(ignore.is_excluded("temp"));
    assert!(ignore.is_excluded("temp.txt"));
}
