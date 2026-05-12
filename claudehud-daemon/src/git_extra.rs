use std::path::Path;
use common::GitExtra;

pub fn read_git_extra(_git_root: &Path) -> GitExtra {
    GitExtra::default()
}
