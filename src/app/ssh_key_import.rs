use crate::session::{config::ManagedKey, ssh_keys::validate_and_inspect};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum KeyImportValidation {
    #[default]
    WaitingForFile,
    Validating,
    Invalid(String),
    Duplicate,
    Valid {
        key_type: String,
        fingerprint: String,
    },
}

impl KeyImportValidation {
    pub(crate) fn can_confirm(&self) -> bool {
        matches!(self, Self::Valid { .. })
    }
}

#[derive(Debug, Default)]
pub(crate) struct KeyImportState {
    pub(crate) open: bool,
    pub(crate) path: String,
    pub(crate) content: String,
    pub(crate) validation: KeyImportValidation,
}

impl KeyImportState {
    pub(crate) fn open(&mut self) {
        self.open = true;
        self.path.clear();
        self.content.clear();
        self.validation = KeyImportValidation::WaitingForFile;
    }

    pub(crate) fn close(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn begin_file_validation(&mut self, path: String) {
        self.path = path;
        self.content.clear();
        self.validation = KeyImportValidation::Validating;
    }

    pub(crate) fn set_file(
        &mut self,
        path: String,
        content: String,
        passphrase: &str,
        managed_keys: &[ManagedKey],
    ) {
        self.path = path;
        self.content = content;
        self.revalidate(passphrase, managed_keys);
    }

    pub(crate) fn set_read_error(&mut self, path: String, error: String) {
        self.path = path;
        self.content.clear();
        self.validation = KeyImportValidation::Invalid(error);
    }

    pub(crate) fn revalidate(&mut self, passphrase: &str, managed_keys: &[ManagedKey]) {
        if matches!(self.validation, KeyImportValidation::Validating) {
            return;
        }
        if self.path.is_empty() {
            self.validation = KeyImportValidation::WaitingForFile;
            return;
        }

        let passphrase = (!passphrase.is_empty()).then_some(passphrase);
        self.validation = match validate_and_inspect(&self.content, passphrase) {
            Ok((key_type, fingerprint)) => {
                if managed_keys
                    .iter()
                    .any(|key| key.fingerprint == fingerprint)
                {
                    KeyImportValidation::Duplicate
                } else {
                    KeyImportValidation::Valid {
                        key_type,
                        fingerprint,
                    }
                }
            }
            Err(err) => KeyImportValidation::Invalid(format!("{err:#}")),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::{KeyImportState, KeyImportValidation};

    #[test]
    fn only_valid_import_can_be_confirmed() {
        assert!(!KeyImportValidation::WaitingForFile.can_confirm());
        assert!(!KeyImportValidation::Validating.can_confirm());
        assert!(!KeyImportValidation::Duplicate.can_confirm());
        assert!(!KeyImportValidation::Invalid("invalid key".into()).can_confirm());
        assert!(
            KeyImportValidation::Valid {
                key_type: "ed25519".into(),
                fingerprint: "SHA256:test".into(),
            }
            .can_confirm()
        );
    }

    #[test]
    fn closing_import_clears_transient_key_material() {
        let mut state = KeyImportState {
            open: true,
            path: "id_ed25519".into(),
            content: "private key".into(),
            validation: KeyImportValidation::Invalid("invalid key".into()),
        };

        state.close();

        assert!(!state.open);
        assert!(state.path.is_empty());
        assert!(state.content.is_empty());
        assert_eq!(state.validation, KeyImportValidation::WaitingForFile);
    }
}
