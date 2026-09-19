use super::*;
use windows_sys::Win32::Globalization::{CSTR_EQUAL, CompareStringOrdinal};

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Installing,
    Active,
    Removing,
    Removed,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    record: Registration,
    phase: Phase,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Backup {
    format: String,
    basename: String,
    original: Option<Value>,
}
pub(super) struct Roots<'a> {
    pub hive: HKEY,
    pub ifeo: &'a str,
    pub product: &'a str,
    pub security: Security,
}
pub(super) enum Security {
    Protected,
    #[cfg(test)]
    Fixture,
}
impl Roots<'_> {
    pub fn machine() -> Self {
        Self {
            hive: HKEY_LOCAL_MACHINE,
            ifeo: IFEO,
            product: PRODUCT,
            security: Security::Protected,
        }
    }
    fn lock(&self) -> Result<Option<security::Mutation>> {
        match self.security {
            Security::Protected => security::Mutation::acquire().map(Some),
            #[cfg(test)]
            Security::Fixture => Ok(None),
        }
    }
    fn owned(&self, key: &Key) -> Result<()> {
        match self.security {
            Security::Protected => security::owned(key),
            #[cfg(test)]
            Security::Fixture => Ok(()),
        }
    }
    fn system(&self, key: &Key) -> Result<()> {
        match self.security {
            Security::Protected => security::system_key(key),
            #[cfg(test)]
            Security::Fixture => Ok(()),
        }
    }
    fn create(&self, parent: HKEY, name: &str) -> Result<(Key, bool)> {
        let descriptor = security::Descriptor::new()?;
        let attributes = descriptor.attributes();
        let attributes = match self.security {
            Security::Protected => Some(&attributes),
            #[cfg(test)]
            Security::Fixture => None,
        };
        let (key, created) = Key::create(parent, name, attributes)?;
        self.owned(&key)?;
        Ok((key, created))
    }
    fn index(&self, create: bool) -> Result<Key> {
        let (parent, leaf) = self
            .product
            .rsplit_once('\\')
            .ok_or(Error::Invalid("IFEO_INDEX_PATH"))?;
        let parent = Key::path(self.hive, parent, create)?
            .ok_or(Error::Invalid("IFEO_INDEX_PARENT_MISSING"))?;
        self.system(&parent)?;
        let product = if create {
            self.create(parent.0, leaf)?.0
        } else {
            Key::open(parent.0, leaf, false)?.ok_or(Error::Invalid("IFEO_NOT_REGISTERED"))?
        };
        self.owned(&product)?;
        let index = if create {
            self.create(product.0, "Ifeo")?.0
        } else {
            Key::open(product.0, "Ifeo", false)?.ok_or(Error::Invalid("IFEO_NOT_REGISTERED"))?
        };
        self.owned(&index)?;
        Ok(index)
    }
    fn parent(&self, record: &Registration, write: bool, create: bool) -> Result<Key> {
        let root =
            Key::path(self.hive, self.ifeo, write)?.ok_or(Error::Invalid("IFEO_ROOT_MISSING"))?;
        self.system(&root)?;
        let parent = match Key::open(root.0, &record.basename()?, write)? {
            Some(key) => key,
            None if create => self.create(root.0, &record.basename()?)?.0,
            None => return Err(Error::Invalid("IFEO_REGISTRATION_CHANGED")),
        };
        self.system(&parent)?;
        Ok(parent)
    }
    fn records(&self, index: &Key, create: bool) -> Result<Option<Key>> {
        let key = if create {
            Some(self.create(index.0, "Registrations")?.0)
        } else {
            Key::open(index.0, "Registrations", false)?
        };
        if let Some(key) = &key {
            self.owned(key)?;
            if !key.children()?.is_empty() {
                return Err(Error::Invalid("IFEO_RECORD_CHANGED"));
            }
        }
        Ok(key)
    }
    fn journal(&self, index: &Key, id: Uuid) -> Result<Journal> {
        let key = self
            .records(index, false)?
            .ok_or(Error::Invalid("IFEO_NOT_REGISTERED"))?;
        self.owned(&key)?;
        let journal: Journal = decode(
            key.value(&id.to_string())?
                .ok_or(Error::Invalid("IFEO_NOT_REGISTERED"))?,
        )?;
        journal.record.validate()?;
        if journal.record.id != id {
            return Err(Error::Invalid("IFEO_RECORD_CHANGED"));
        }
        Ok(journal)
    }
    fn journals(&self, index: &Key) -> Result<Vec<Journal>> {
        let Some(records) = self.records(index, false)? else {
            return Ok(Vec::new());
        };
        records
            .value_names()?
            .into_iter()
            .map(|name| {
                let id =
                    Uuid::parse_str(&name).map_err(|_| Error::Invalid("IFEO_INVALID_INDEX"))?;
                if id.to_string() != name {
                    return Err(Error::Invalid("IFEO_INVALID_INDEX"));
                }
                self.journal(index, id)
            })
            .collect()
    }
    pub fn read_for_removal(&self, id: Uuid) -> Result<Registration> {
        if id.is_nil() {
            return Err(Error::Invalid("IFEO_INVALID_ID"));
        }
        Ok(self.journal(&self.index(false)?, id)?.record)
    }
    pub fn read(&self, id: Uuid) -> Result<Registration> {
        let journal = self.journal(&self.index(false)?, id)?;
        if journal.phase != Phase::Active {
            return Err(Error::Invalid(if journal.phase == Phase::Removed {
                "IFEO_NOT_REGISTERED"
            } else {
                "IFEO_OPERATION_INCOMPLETE"
            }));
        }
        Ok(journal.record)
    }
    pub fn verify(&self, record: &Registration) -> Result<()> {
        self.exact(&self.parent(record, false, false)?, record)
    }
    fn expected(&self, record: &Registration) -> Result<[(&'static str, Value); 3]> {
        Ok([
            ("AppProxyRustOwner", Value::string(&record.id.to_string())?),
            ("FilterFullPath", Value::string(&dos_path(&record.target)?)?),
            ("Debugger", Value::string(&record.debugger()?)?),
        ])
    }
    fn filter(&self, parent: &Key, record: &Registration, partial: bool) -> Result<Option<Key>> {
        let Some(filter) = Key::open(parent.0, &record.filter(), false)? else {
            return if partial {
                Ok(None)
            } else {
                Err(Error::Invalid("IFEO_REGISTRATION_CHANGED"))
            };
        };
        self.owned(&filter)?;
        let mut count = 0;
        for (name, expected) in self.expected(record)? {
            match filter.value(name)? {
                Some(value) if value == expected => count += 1,
                None if partial => {}
                _ => return Err(Error::Invalid("IFEO_REGISTRATION_CHANGED")),
            }
        }
        if filter.value_count()? != count || !filter.children()?.is_empty() {
            return Err(Error::Invalid("IFEO_REGISTRATION_CHANGED"));
        }
        Ok(Some(filter))
    }
    fn exact(&self, parent: &Key, record: &Registration) -> Result<()> {
        if parent.value("Debugger")?.is_some()
            || parent.value("UseFilter")? != Some(Value::dword(1))
        {
            return Err(Error::Invalid("IFEO_REGISTRATION_CHANGED"));
        }
        self.filter(parent, record, false)?;
        self.foreign(parent, record, &Some(Value::dword(1)))?;
        Ok(())
    }
    fn backup(&self, index: &Key, record: &Registration) -> Result<Backup> {
        let backup: Backup = decode(
            index
                .value(&format!("Parent-{}", record.basename()?))?
                .ok_or(Error::Invalid("IFEO_BACKUP_MISSING"))?,
        )?;
        if backup.format != "app-proxy-rust-ifeo-parent-v1"
            || !same_name(&backup.basename, &record.basename()?)?
        {
            return Err(Error::Invalid("IFEO_INVALID_BACKUP"));
        }
        valid_filter(&backup.original)?;
        Ok(backup)
    }
    fn foreign(&self, parent: &Key, record: &Registration, original: &Option<Value>) -> Result<()> {
        if parent.value("Debugger")?.is_some() {
            return Err(Error::Invalid("IFEO_FOREIGN_DEBUGGER"));
        }
        for name in parent.children()? {
            if same_name(&name, &record.filter())? {
                continue;
            }
            if original != &Some(Value::dword(1)) {
                return Err(Error::Invalid("IFEO_INACTIVE_FOREIGN_FILTERS"));
            }
            let filter = Key::open(parent.0, &name, false)?
                .ok_or(Error::Invalid("IFEO_REGISTRATION_CHANGED"))?;
            let path = filter
                .value("FilterFullPath")?
                .ok_or(Error::Invalid("IFEO_UNKNOWN_FILTER"))?
                .text()?;
            if same_name(&dos_path(Path::new(&path))?, &dos_path(&record.target)?)? {
                return Err(Error::Invalid("IFEO_FILTER_CONFLICT"));
            }
        }
        Ok(())
    }
    pub fn install(&self, record: &Registration) -> Result<()> {
        record.validate()?;
        let intent = Journal {
            record: record.clone(),
            phase: Phase::Installing,
        };
        encode(&intent)?;
        let _lock = self.lock()?;
        let index = self.index(true)?;
        let journals = self.journals(&index)?;
        let existing = journals.iter().find(|j| j.record.id == record.id);
        if existing.is_none() && journals.len() >= 512 {
            return Err(Error::Invalid("IFEO_RECORD_LIMIT"));
        }
        if let Some(journal) = existing {
            if encode(&journal.record)? != encode(record)? {
                return Err(Error::Invalid("IFEO_OWNER_CONFLICT"));
            }
            match journal.phase {
                Phase::Active => return self.verify(record),
                Phase::Installing => {}
                _ => return Err(Error::Invalid("IFEO_REGISTRATION_RETIRED")),
            }
        }
        let mut participants = 0;
        for journal in &journals {
            if journal.phase == Phase::Removed {
                continue;
            }
            let prior = &journal.record;
            if same_name(&prior.basename()?, &record.basename()?)? {
                participants += 1;
            }
            if prior.id != record.id
                && (prior.target_image == record.target_image
                    || same_name(&dos_path(&prior.target)?, &dos_path(&record.target)?)?)
            {
                return Err(Error::Invalid("IFEO_OWNER_CONFLICT"));
            }
        }
        let parent = self.parent(record, true, true)?;
        let existing_filter = Key::open(parent.0, &record.filter(), false)?;
        if existing.is_none() && existing_filter.is_some() {
            return Err(Error::Invalid("IFEO_OWNER_CONFLICT"));
        }
        if existing_filter.is_none() && parent.children()?.len() >= 512 {
            return Err(Error::Invalid("IFEO_KEY_LIMIT"));
        }
        let current = parent.value("UseFilter")?;
        valid_filter(&current)?;
        let backup = if participants == 0 {
            self.foreign(&parent, record, &current)?;
            let backup = Backup {
                format: "app-proxy-rust-ifeo-parent-v1".into(),
                basename: record.basename()?,
                original: current.clone(),
            };
            // No live journal means the previous round has completed. Capture
            // the current parent, never reuse an obsolete round's original value.
            save(
                &index,
                &format!("Parent-{}", record.basename()?),
                &encode(&backup)?,
            )?;
            backup
        } else {
            self.backup(&index, record)?
        };
        if current != backup.original && current != Some(Value::dword(1)) {
            return Err(Error::Invalid("IFEO_PARENT_CHANGED"));
        }
        // Other owned participants already rely on UseFilter=1. Their filters
        // were validated when registered; all foreign paths still require checks.
        self.foreign(&parent, record, &current)?;
        let entry = self
            .records(&index, true)?
            .ok_or(Error::Invalid("IFEO_NOT_REGISTERED"))?;
        if existing.is_none() {
            if entry.value(&record.id.to_string())?.is_some() {
                return Err(Error::Invalid("IFEO_OWNER_CONFLICT"));
            }
            // Publishing one value has no per-registration empty-key window.
            // An interrupted first write leaves either no intent or a complete
            // intent; no filter is touched before this value is flushed.
            before_intent()?;
            save(&entry, &record.id.to_string(), &encode(&intent)?)?;
        }
        self.filter(&parent, record, true)?;
        let (filter, _) = self.create(parent.0, &record.filter())?;
        // Durable intent precedes the first filter write. Debugger is always last.
        for (name, value) in self.expected(record)?.into_iter().take(2) {
            ensure(&filter, name, &value)?;
        }
        let actual = parent.value("UseFilter")?;
        if actual != backup.original && actual != Some(Value::dword(1)) {
            return Err(Error::Invalid("IFEO_PARENT_CHANGED"));
        }
        self.foreign(&parent, record, &actual)?;
        save(&parent, "UseFilter", &Value::dword(1))?;
        ensure(&filter, "Debugger", &Value::string(&record.debugger()?)?)?;
        self.exact(&parent, record)?;
        save(
            &entry,
            &record.id.to_string(),
            &encode(&Journal {
                record: record.clone(),
                phase: Phase::Active,
            })?,
        )?;
        self.verify(record)
    }
    pub fn remove(&self, record: &Registration) -> Result<()> {
        let _lock = self.lock()?;
        let index = self.index(false)?;
        let journal = self.journal(&index, record.id)?;
        if encode(&journal.record)? != encode(record)? {
            return Err(Error::Invalid("IFEO_OWNER_CONFLICT"));
        }
        let parent = self.parent(record, true, false)?;
        if journal.phase == Phase::Removed {
            if Key::open(parent.0, &record.filter(), false)?.is_some() {
                return Err(Error::Invalid("IFEO_REGISTRATION_CHANGED"));
            }
            return Ok(());
        }
        if parent.value("Debugger")?.is_some() {
            return Err(Error::Invalid("IFEO_FOREIGN_DEBUGGER"));
        }
        let backup = self.backup(&index, record)?;
        let current = parent.value("UseFilter")?;
        if current != Some(Value::dword(1))
            && !(journal.phase != Phase::Active && current == backup.original)
        {
            return Err(Error::Invalid("IFEO_PARENT_CHANGED"));
        }
        self.filter(&parent, record, journal.phase != Phase::Active)?;
        let entry = self
            .records(&index, true)?
            .ok_or(Error::Invalid("IFEO_NOT_REGISTERED"))?;
        save(
            &entry,
            &record.id.to_string(),
            &encode(&Journal {
                record: record.clone(),
                phase: Phase::Removing,
            })?,
        )?;
        if let Some(filter) = self.filter(&parent, record, true)? {
            if filter.value("Debugger")?.is_some() {
                let filter = Key::open(parent.0, &record.filter(), true)?
                    .ok_or(Error::Invalid("IFEO_REGISTRATION_CHANGED"))?;
                if filter.value("Debugger")? != Some(Value::string(&record.debugger()?)?) {
                    return Err(Error::Invalid("IFEO_REGISTRATION_CHANGED"));
                }
                filter.remove_value("Debugger")?;
                filter.flush()?;
                checkpoint()?;
            }
            self.filter(&parent, record, true)?;
            parent.delete_child(&record.filter())?;
            parent.flush()?;
            checkpoint()?;
        }
        let mut last = true;
        for other in self.journals(&index)? {
            if other.record.id != record.id
                && other.phase != Phase::Removed
                && same_name(&other.record.basename()?, &record.basename()?)?
            {
                last = false;
            }
        }
        if last {
            if backup.original != Some(Value::dword(1)) && !parent.children()?.is_empty() {
                return Err(Error::Invalid("IFEO_FOREIGN_FILTERS_ADDED"));
            }
            let current = parent.value("UseFilter")?;
            if current != backup.original {
                if current != Some(Value::dword(1)) {
                    return Err(Error::Invalid("IFEO_PARENT_CHANGED"));
                }
                match &backup.original {
                    Some(value) => save(&parent, "UseFilter", value)?,
                    None => {
                        parent.remove_value("UseFilter")?;
                        parent.flush()?;
                        checkpoint()?;
                    }
                }
            }
        }
        save(
            &entry,
            &record.id.to_string(),
            &encode(&Journal {
                record: record.clone(),
                phase: Phase::Removed,
            })?,
        )?;
        if Key::open(parent.0, &record.filter(), false)?.is_some() {
            return Err(Error::Invalid("IFEO_REMOVE_UNCONFIRMED"));
        }
        Ok(())
    }
}
fn valid_filter(value: &Option<Value>) -> Result<()> {
    if value
        .as_ref()
        .is_some_and(|v| *v != Value::dword(0) && *v != Value::dword(1))
    {
        return Err(Error::Invalid("IFEO_INVALID_USE_FILTER"));
    }
    Ok(())
}
fn ensure(key: &Key, name: &str, value: &Value) -> Result<()> {
    match key.value(name)? {
        Some(actual) if &actual == value => Ok(()),
        None => save(key, name, value),
        _ => Err(Error::Invalid("IFEO_REGISTRATION_CHANGED")),
    }
}
fn save(key: &Key, name: &str, value: &Value) -> Result<()> {
    key.set(name, value)?;
    key.flush()?;
    checkpoint()
}
#[cfg(test)]
thread_local! { static INTERRUPT: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) }; }
#[cfg(test)]
thread_local! { static BEFORE_INTENT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
#[cfg(test)]
pub(super) fn interrupt_before_intent() {
    BEFORE_INTENT.with(|value| value.set(true));
}
fn before_intent() -> Result<()> {
    #[cfg(test)]
    if BEFORE_INTENT.with(|value| value.replace(false)) {
        return Err(Error::Invalid("IFEO_FIXTURE_INTERRUPTION"));
    }
    Ok(())
}
#[cfg(test)]
pub(super) fn interrupt_after(step: usize) {
    INTERRUPT.with(|value| value.set(Some(step)));
}
fn checkpoint() -> Result<()> {
    #[cfg(test)]
    if INTERRUPT.with(|value| match value.get() {
        Some(1) => {
            value.set(None);
            true
        }
        Some(n) => {
            value.set(Some(n - 1));
            false
        }
        None => false,
    }) {
        return Err(Error::Invalid("IFEO_FIXTURE_INTERRUPTION"));
    }
    Ok(())
}
pub(super) fn encode(value: &impl Serialize) -> Result<Value> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > 65536 {
        return Err(Error::Invalid("IFEO_RECORD_SIZE"));
    }
    Ok(Value {
        kind: REG_BINARY,
        bytes,
    })
}
pub(super) fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T> {
    if value.kind != REG_BINARY || value.bytes.len() > 65536 {
        return Err(Error::Invalid("IFEO_INVALID_RECORD"));
    }
    serde_json::from_slice(&value.bytes).map_err(|_| Error::Invalid("IFEO_INVALID_RECORD"))
}
pub(super) fn same_name(a: &str, b: &str) -> Result<bool> {
    let a: Vec<_> = a.encode_utf16().collect();
    let b: Vec<_> = b.encode_utf16().collect();
    // SAFETY: both UTF-16 buffers remain live for their explicitly supplied lengths.
    let result =
        unsafe { CompareStringOrdinal(a.as_ptr(), a.len() as i32, b.as_ptr(), b.len() as i32, 1) };
    if result == 0 {
        return Err(crate::last_error("CompareIfeoPath"));
    }
    Ok(result == CSTR_EQUAL)
}
