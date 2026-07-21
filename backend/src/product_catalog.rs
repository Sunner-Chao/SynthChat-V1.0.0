use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value as JsonValue, json};
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

const DATABASE_DIR: &str = ".synthchat";
const DATABASE_FILE: &str = "product-catalog-v1.db";
const MAX_ITEMS_PER_KIND: i64 = 2_000;
const MAX_NAME_CHARS: usize = 120;
const MAX_PROMPT_CHARS: usize = 64_000;
const MAX_BODY_CHARS: usize = 16_000;
const MAX_DESCRIPTION_CHARS: usize = 8_000;
const MAX_SECTIONS: usize = 200;
const MAX_COMMENTS: usize = 1_000;
const MAX_BINDINGS: usize = 200;
pub(crate) const MAX_RUN_PERSONA_CONTEXT_CHARS: usize = 128 * 1024;

#[derive(Clone)]
pub struct ProductCatalogService {
    database_path: Arc<PathBuf>,
    process_lock: Arc<Mutex<()>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RecordKind {
    Persona,
    Moment,
    Worldbook,
}

impl RecordKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Persona => "persona",
            Self::Moment => "moment",
            Self::Worldbook => "worldbook",
        }
    }

    const fn id_prefix(self) -> &'static str {
        match self {
            Self::Persona => "persona",
            Self::Moment => "moment",
            Self::Worldbook => "worldbook",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PersonaInput {
    pub name: String,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default = "default_system_prompt")]
    pub system_prompt: String,
    #[serde(default)]
    pub character_prompt: String,
    #[serde(default)]
    pub output_examples: String,
    #[serde(default = "default_system_instructions")]
    pub system_instructions: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    #[serde(default = "default_temperature")]
    pub temperature: f64,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default = "enabled")]
    pub tools_enabled: bool,
    #[serde(default = "enabled")]
    pub memory_enabled: bool,
    #[serde(default)]
    pub proactive_enabled: bool,
    #[serde(default)]
    pub legacy_agent_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Persona {
    pub id: String,
    #[serde(flatten)]
    pub value: PersonaInput,
    pub created_at: String,
    pub updated_at: String,
    pub revision: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorldbookSectionInput {
    pub key: String,
    pub content: String,
    #[serde(default = "enabled")]
    pub enabled: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorldbookSection {
    pub id: String,
    #[serde(flatten)]
    pub value: WorldbookSectionInput,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorldbookInput {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub bound_persona_ids: Vec<String>,
    #[serde(default)]
    pub sections: Vec<WorldbookSectionInput>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Worldbook {
    pub id: String,
    pub name: String,
    pub description: String,
    pub bound_persona_ids: Vec<String>,
    pub sections: Vec<WorldbookSection>,
    pub created_at: String,
    pub updated_at: String,
    pub revision: u64,
}

/// Immutable, bounded input projected into a Run after profile-scoped lookup.
/// It intentionally excludes mutable catalog revision details and every field that
/// is not part of inference policy or system context.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RunPersonaSnapshot {
    pub(crate) id: String,
    pub(crate) provider: String,
    pub(crate) model: String,
    pub(crate) tools_enabled: bool,
    pub(crate) memory_enabled: bool,
    pub(crate) system_context: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MomentComment {
    pub id: String,
    pub author_id: String,
    pub text: String,
    pub reply_to: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MomentInput {
    #[serde(default = "default_author")]
    pub author_id: String,
    pub body: String,
    #[serde(default)]
    pub cover_file_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Moment {
    pub id: String,
    pub author_id: String,
    pub body: String,
    pub cover_file_id: Option<String>,
    pub liked_by: Vec<String>,
    pub comments: Vec<MomentComment>,
    pub created_at: String,
    pub updated_at: String,
    pub revision: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MomentCommentInput {
    #[serde(default = "default_author")]
    pub author_id: String,
    pub text: String,
    #[serde(default)]
    pub reply_to: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MomentLikeInput {
    #[serde(default = "default_author")]
    pub actor_id: String,
    pub liked: bool,
}

#[derive(Debug, Error)]
pub enum ProductCatalogError {
    #[error("invalid product catalog request")]
    InvalidRequest,
    #[error("product catalog item not found")]
    NotFound,
    #[error("product catalog revision conflict")]
    RevisionConflict { current_revision: u64 },
    #[error("product catalog storage is unavailable")]
    StorageUnavailable,
    #[error("product catalog limit reached")]
    LimitReached,
}

impl ProductCatalogService {
    pub fn new(hermes_home: &Path) -> Self {
        Self {
            database_path: Arc::new(hermes_home.join(DATABASE_DIR).join(DATABASE_FILE)),
            process_lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn list_personas(
        &self,
        profile_id: &str,
        query: Option<&str>,
    ) -> Result<Vec<Persona>, ProductCatalogError> {
        self.list(profile_id, RecordKind::Persona, query)
    }

    pub fn get_persona(&self, profile_id: &str, id: &str) -> Result<Persona, ProductCatalogError> {
        self.get(profile_id, RecordKind::Persona, id)
    }

    /// Reads a Persona and all of its enabled bound Worldbook sections from one
    /// SQLite snapshot. The Run service can therefore use this method without
    /// ever accepting prompt text or catalog identifiers from another Profile.
    pub(crate) fn run_persona_snapshot(
        &self,
        profile_id: &str,
        persona_id: &str,
    ) -> Result<RunPersonaSnapshot, ProductCatalogError> {
        if invalid_id(persona_id) {
            return Err(ProductCatalogError::InvalidRequest);
        }
        let _guard = self.lock()?;
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(map_storage)?;
        let raw = select_record(&transaction, profile_id, RecordKind::Persona, persona_id)?
            .ok_or(ProductCatalogError::NotFound)?;
        let persona: Persona = deserialize_record(raw)?;
        let sections = {
            let mut statement = transaction
                .prepare(
                    "SELECT payload_json, id, created_at, updated_at, revision FROM product_records \
                     WHERE profile_id = ?1 AND kind = 'worldbook' \
                     ORDER BY updated_at DESC, id ASC",
                )
                .map_err(map_storage)?;
            let rows = statement
                .query_map(params![profile_id], |row| {
                    Ok(RawRecord {
                        payload_json: row.get(0)?,
                        id: row.get(1)?,
                        created_at: row.get(2)?,
                        updated_at: row.get(3)?,
                        revision: row.get(4)?,
                    })
                })
                .map_err(map_storage)?;
            let mut sections = Vec::new();
            for row in rows {
                let worldbook: Worldbook = deserialize_record(row.map_err(map_storage)?)?;
                if !worldbook
                    .bound_persona_ids
                    .iter()
                    .any(|bound_id| bound_id == persona_id)
                {
                    continue;
                }
                for section in worldbook
                    .sections
                    .iter()
                    .filter(|section| section.value.enabled)
                {
                    sections.push(json!({
                        "worldbookId": worldbook.id.clone(),
                        "worldbookName": worldbook.name.clone(),
                        "sectionId": section.id.clone(),
                        "key": section.value.key.clone(),
                        "content": section.value.content.clone(),
                    }));
                }
            }
            sections
        };
        let system_context = run_persona_system_context(&persona, sections)?;
        transaction.commit().map_err(map_storage)?;
        Ok(RunPersonaSnapshot {
            id: persona.id,
            provider: persona.value.provider.trim().to_owned(),
            model: persona.value.model.trim().to_owned(),
            tools_enabled: persona.value.tools_enabled,
            memory_enabled: persona.value.memory_enabled,
            system_context,
        })
    }

    pub fn create_persona(
        &self,
        profile_id: &str,
        input: &PersonaInput,
    ) -> Result<Persona, ProductCatalogError> {
        validate_persona(input)?;
        self.create(profile_id, RecordKind::Persona, input.name.trim(), input)
    }

    pub fn update_persona(
        &self,
        profile_id: &str,
        id: &str,
        expected_revision: u64,
        input: &PersonaInput,
    ) -> Result<Persona, ProductCatalogError> {
        validate_persona(input)?;
        self.update(
            profile_id,
            RecordKind::Persona,
            id,
            expected_revision,
            input.name.trim(),
            input,
        )
    }

    pub fn delete_persona(
        &self,
        profile_id: &str,
        id: &str,
        expected_revision: u64,
    ) -> Result<(), ProductCatalogError> {
        let _guard = self.lock()?;
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_storage)?;
        let revision = select_revision(&transaction, profile_id, RecordKind::Persona, id)?;
        require_revision(revision, expected_revision)?;
        let bound = {
            let mut statement = transaction
                .prepare(
                    "SELECT payload_json FROM product_records
                     WHERE profile_id = ?1 AND kind = 'worldbook'",
                )
                .map_err(map_storage)?;
            let rows = statement
                .query_map(params![profile_id], |row| row.get::<_, String>(0))
                .map_err(map_storage)?;
            let mut bound = false;
            for row in rows {
                let payload = row.map_err(map_storage)?;
                let book: Worldbook = serde_json::from_str(&payload)
                    .map_err(|_| ProductCatalogError::StorageUnavailable)?;
                if book.bound_persona_ids.iter().any(|item| item == id) {
                    bound = true;
                    break;
                }
            }
            bound
        };
        if bound {
            return Err(ProductCatalogError::InvalidRequest);
        }
        delete_record(&transaction, profile_id, RecordKind::Persona, id)?;
        transaction.commit().map_err(map_storage)
    }

    pub fn list_worldbooks(
        &self,
        profile_id: &str,
        query: Option<&str>,
    ) -> Result<Vec<Worldbook>, ProductCatalogError> {
        self.list(profile_id, RecordKind::Worldbook, query)
    }

    pub fn get_worldbook(
        &self,
        profile_id: &str,
        id: &str,
    ) -> Result<Worldbook, ProductCatalogError> {
        self.get(profile_id, RecordKind::Worldbook, id)
    }

    pub fn create_worldbook(
        &self,
        profile_id: &str,
        input: &WorldbookInput,
    ) -> Result<Worldbook, ProductCatalogError> {
        validate_worldbook(input)?;
        self.ensure_personas_exist(profile_id, &input.bound_persona_ids)?;
        let now = timestamp()?;
        let book = worldbook_from_input(String::new(), input, now.clone(), now, 0);
        self.create(profile_id, RecordKind::Worldbook, input.name.trim(), &book)
    }

    pub fn update_worldbook(
        &self,
        profile_id: &str,
        id: &str,
        expected_revision: u64,
        input: &WorldbookInput,
    ) -> Result<Worldbook, ProductCatalogError> {
        validate_worldbook(input)?;
        self.ensure_personas_exist(profile_id, &input.bound_persona_ids)?;
        let current = self.get_worldbook(profile_id, id)?;
        let book = worldbook_from_input(
            id.to_owned(),
            input,
            current.created_at,
            current.updated_at,
            current.revision,
        );
        self.update(
            profile_id,
            RecordKind::Worldbook,
            id,
            expected_revision,
            input.name.trim(),
            &book,
        )
    }

    pub fn delete_worldbook(
        &self,
        profile_id: &str,
        id: &str,
        expected_revision: u64,
    ) -> Result<(), ProductCatalogError> {
        self.delete(profile_id, RecordKind::Worldbook, id, expected_revision)
    }

    pub fn list_moments(&self, profile_id: &str) -> Result<Vec<Moment>, ProductCatalogError> {
        self.list(profile_id, RecordKind::Moment, None)
    }

    pub fn get_moment(&self, profile_id: &str, id: &str) -> Result<Moment, ProductCatalogError> {
        self.get(profile_id, RecordKind::Moment, id)
    }

    pub fn create_moment(
        &self,
        profile_id: &str,
        input: &MomentInput,
    ) -> Result<Moment, ProductCatalogError> {
        validate_moment(input)?;
        let now = timestamp()?;
        let moment = Moment {
            id: String::new(),
            author_id: input.author_id.trim().to_owned(),
            body: input.body.trim().to_owned(),
            cover_file_id: normalized_optional(&input.cover_file_id),
            liked_by: Vec::new(),
            comments: Vec::new(),
            created_at: now.clone(),
            updated_at: now,
            revision: 0,
        };
        self.create(
            profile_id,
            RecordKind::Moment,
            moment.author_id.as_str(),
            &moment,
        )
    }

    pub fn update_moment(
        &self,
        profile_id: &str,
        id: &str,
        expected_revision: u64,
        input: &MomentInput,
    ) -> Result<Moment, ProductCatalogError> {
        validate_moment(input)?;
        let mut current = self.get_moment(profile_id, id)?;
        current.author_id = input.author_id.trim().to_owned();
        current.body = input.body.trim().to_owned();
        current.cover_file_id = normalized_optional(&input.cover_file_id);
        self.update(
            profile_id,
            RecordKind::Moment,
            id,
            expected_revision,
            current.author_id.as_str(),
            &current,
        )
    }

    pub fn add_moment_comment(
        &self,
        profile_id: &str,
        id: &str,
        expected_revision: u64,
        input: &MomentCommentInput,
    ) -> Result<Moment, ProductCatalogError> {
        if invalid_text(&input.author_id, MAX_NAME_CHARS)
            || invalid_text(&input.text, MAX_BODY_CHARS)
            || input
                .reply_to
                .as_ref()
                .is_some_and(|value| invalid_id(value))
        {
            return Err(ProductCatalogError::InvalidRequest);
        }
        self.mutate_moment(profile_id, id, expected_revision, |moment, now| {
            if moment.comments.len() >= MAX_COMMENTS {
                return Err(ProductCatalogError::LimitReached);
            }
            if let Some(reply_to) = input.reply_to.as_deref()
                && !moment.comments.iter().any(|item| item.id == reply_to)
            {
                return Err(ProductCatalogError::InvalidRequest);
            }
            moment.comments.push(MomentComment {
                id: generated_id("comment"),
                author_id: input.author_id.trim().to_owned(),
                text: input.text.trim().to_owned(),
                reply_to: normalized_optional(&input.reply_to),
                created_at: now.clone(),
                updated_at: now,
            });
            Ok(())
        })
    }

    pub fn delete_moment_comment(
        &self,
        profile_id: &str,
        id: &str,
        comment_id: &str,
        expected_revision: u64,
    ) -> Result<Moment, ProductCatalogError> {
        if invalid_id(comment_id) {
            return Err(ProductCatalogError::InvalidRequest);
        }
        self.mutate_moment(profile_id, id, expected_revision, |moment, _| {
            let before = moment.comments.len();
            moment.comments.retain(|item| item.id != comment_id);
            if before == moment.comments.len() {
                return Err(ProductCatalogError::NotFound);
            }
            for comment in &mut moment.comments {
                if comment.reply_to.as_deref() == Some(comment_id) {
                    comment.reply_to = None;
                }
            }
            Ok(())
        })
    }

    pub fn set_moment_like(
        &self,
        profile_id: &str,
        id: &str,
        expected_revision: u64,
        input: &MomentLikeInput,
    ) -> Result<Moment, ProductCatalogError> {
        if invalid_text(&input.actor_id, MAX_NAME_CHARS) {
            return Err(ProductCatalogError::InvalidRequest);
        }
        self.mutate_moment(profile_id, id, expected_revision, |moment, _| {
            moment.liked_by.retain(|item| item != input.actor_id.trim());
            if input.liked {
                moment.liked_by.push(input.actor_id.trim().to_owned());
            }
            Ok(())
        })
    }

    pub fn delete_moment(
        &self,
        profile_id: &str,
        id: &str,
        expected_revision: u64,
    ) -> Result<(), ProductCatalogError> {
        self.delete(profile_id, RecordKind::Moment, id, expected_revision)
    }

    fn list<T: DeserializeOwned>(
        &self,
        profile_id: &str,
        kind: RecordKind,
        query: Option<&str>,
    ) -> Result<Vec<T>, ProductCatalogError> {
        let query = query.unwrap_or("").trim();
        if query.chars().count() > 200 || query.chars().any(char::is_control) {
            return Err(ProductCatalogError::InvalidRequest);
        }
        let _guard = self.lock()?;
        let connection = self.connection()?;
        let pattern = format!("%{}%", escape_like(query));
        let mut statement = connection
            .prepare(
                "SELECT payload_json, id, created_at, updated_at, revision
                 FROM product_records
                 WHERE profile_id = ?1 AND kind = ?2
                   AND (?3 = '' OR name LIKE ?4 ESCAPE '\\' COLLATE NOCASE OR payload_json LIKE ?4 ESCAPE '\\' COLLATE NOCASE)
                 ORDER BY updated_at DESC, id ASC
                 LIMIT ?5",
            )
            .map_err(map_storage)?;
        let rows = statement
            .query_map(
                params![
                    profile_id,
                    kind.as_str(),
                    query,
                    pattern,
                    MAX_ITEMS_PER_KIND
                ],
                |row| {
                    Ok(RawRecord {
                        payload_json: row.get(0)?,
                        id: row.get(1)?,
                        created_at: row.get(2)?,
                        updated_at: row.get(3)?,
                        revision: row.get::<_, i64>(4)?,
                    })
                },
            )
            .map_err(map_storage)?;
        rows.map(|row| deserialize_record(row.map_err(map_storage)?))
            .collect()
    }

    fn get<T: DeserializeOwned>(
        &self,
        profile_id: &str,
        kind: RecordKind,
        id: &str,
    ) -> Result<T, ProductCatalogError> {
        if invalid_id(id) {
            return Err(ProductCatalogError::InvalidRequest);
        }
        let _guard = self.lock()?;
        let connection = self.connection()?;
        let record = select_record(&connection, profile_id, kind, id)?
            .ok_or(ProductCatalogError::NotFound)?;
        deserialize_record(record)
    }

    fn create<T: Serialize, O: DeserializeOwned>(
        &self,
        profile_id: &str,
        kind: RecordKind,
        name: &str,
        value: &T,
    ) -> Result<O, ProductCatalogError> {
        let _guard = self.lock()?;
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_storage)?;
        let count = transaction
            .query_row(
                "SELECT count(*) FROM product_records WHERE profile_id = ?1 AND kind = ?2",
                params![profile_id, kind.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .map_err(map_storage)?;
        if count >= MAX_ITEMS_PER_KIND {
            return Err(ProductCatalogError::LimitReached);
        }
        let id = generated_id(kind.id_prefix());
        let now = timestamp()?;
        let payload =
            serde_json::to_value(value).map_err(|_| ProductCatalogError::InvalidRequest)?;
        let payload = stamp_payload(payload, &id, &now, &now, 1)?;
        let payload_json =
            serde_json::to_string(&payload).map_err(|_| ProductCatalogError::InvalidRequest)?;
        transaction
            .execute(
                "INSERT INTO product_records(profile_id, kind, id, name, payload_json, created_at, updated_at, revision)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, 1)",
                params![profile_id, kind.as_str(), id, name, payload_json, now],
            )
            .map_err(map_storage)?;
        transaction.commit().map_err(map_storage)?;
        serde_json::from_value(payload).map_err(|_| ProductCatalogError::StorageUnavailable)
    }

    fn update<T: Serialize, O: DeserializeOwned>(
        &self,
        profile_id: &str,
        kind: RecordKind,
        id: &str,
        expected_revision: u64,
        name: &str,
        value: &T,
    ) -> Result<O, ProductCatalogError> {
        if invalid_id(id) {
            return Err(ProductCatalogError::InvalidRequest);
        }
        let _guard = self.lock()?;
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_storage)?;
        let current = select_record(&transaction, profile_id, kind, id)?
            .ok_or(ProductCatalogError::NotFound)?;
        require_revision(current.revision, expected_revision)?;
        let revision = expected_revision
            .checked_add(1)
            .ok_or(ProductCatalogError::StorageUnavailable)?;
        let now = timestamp()?;
        let payload =
            serde_json::to_value(value).map_err(|_| ProductCatalogError::InvalidRequest)?;
        let payload = stamp_payload(payload, id, &current.created_at, &now, revision)?;
        let payload_json =
            serde_json::to_string(&payload).map_err(|_| ProductCatalogError::InvalidRequest)?;
        let changed = transaction
            .execute(
                "UPDATE product_records SET name = ?1, payload_json = ?2, updated_at = ?3, revision = ?4
                 WHERE profile_id = ?5 AND kind = ?6 AND id = ?7 AND revision = ?8",
                params![name, payload_json, now, revision, profile_id, kind.as_str(), id, expected_revision],
            )
            .map_err(map_storage)?;
        if changed != 1 {
            return Err(ProductCatalogError::RevisionConflict {
                current_revision: u64::try_from(current.revision)
                    .map_err(|_| ProductCatalogError::StorageUnavailable)?,
            });
        }
        transaction.commit().map_err(map_storage)?;
        serde_json::from_value(payload).map_err(|_| ProductCatalogError::StorageUnavailable)
    }

    fn delete(
        &self,
        profile_id: &str,
        kind: RecordKind,
        id: &str,
        expected_revision: u64,
    ) -> Result<(), ProductCatalogError> {
        if invalid_id(id) {
            return Err(ProductCatalogError::InvalidRequest);
        }
        let _guard = self.lock()?;
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_storage)?;
        let revision = select_revision(&transaction, profile_id, kind, id)?;
        require_revision(revision, expected_revision)?;
        delete_record(&transaction, profile_id, kind, id)?;
        transaction.commit().map_err(map_storage)
    }

    fn mutate_moment<F>(
        &self,
        profile_id: &str,
        id: &str,
        expected_revision: u64,
        mutate: F,
    ) -> Result<Moment, ProductCatalogError>
    where
        F: FnOnce(&mut Moment, String) -> Result<(), ProductCatalogError>,
    {
        let mut moment = self.get_moment(profile_id, id)?;
        require_revision(moment.revision as i64, expected_revision)?;
        let now = timestamp()?;
        mutate(&mut moment, now)?;
        let name = moment.author_id.clone();
        self.update(
            profile_id,
            RecordKind::Moment,
            id,
            expected_revision,
            &name,
            &moment,
        )
    }

    fn connection(&self) -> Result<Connection, ProductCatalogError> {
        if let Some(parent) = self.database_path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| ProductCatalogError::StorageUnavailable)?;
        }
        let connection = Connection::open(self.database_path.as_ref()).map_err(map_storage)?;
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA foreign_keys=ON;
                 PRAGMA busy_timeout=5000;
                 CREATE TABLE IF NOT EXISTS product_records (
                   profile_id TEXT NOT NULL,
                   kind TEXT NOT NULL CHECK(kind IN ('persona', 'moment', 'worldbook')),
                   id TEXT NOT NULL,
                   name TEXT NOT NULL,
                   payload_json TEXT NOT NULL CHECK(json_valid(payload_json)),
                   created_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   revision INTEGER NOT NULL CHECK(revision > 0),
                   PRIMARY KEY(profile_id, kind, id)
                 );
                 CREATE INDEX IF NOT EXISTS product_records_list_idx
                   ON product_records(profile_id, kind, updated_at DESC);",
            )
            .map_err(map_storage)?;
        Ok(connection)
    }

    fn ensure_personas_exist(
        &self,
        profile_id: &str,
        persona_ids: &[String],
    ) -> Result<(), ProductCatalogError> {
        if persona_ids.is_empty() {
            return Ok(());
        }
        let _guard = self.lock()?;
        let connection = self.connection()?;
        for id in persona_ids {
            if select_record(&connection, profile_id, RecordKind::Persona, id)?.is_none() {
                return Err(ProductCatalogError::InvalidRequest);
            }
        }
        Ok(())
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, ()>, ProductCatalogError> {
        self.process_lock
            .lock()
            .map_err(|_| ProductCatalogError::StorageUnavailable)
    }
}

#[derive(Debug)]
struct RawRecord {
    payload_json: String,
    id: String,
    created_at: String,
    updated_at: String,
    revision: i64,
}

fn select_record(
    connection: &Connection,
    profile_id: &str,
    kind: RecordKind,
    id: &str,
) -> Result<Option<RawRecord>, ProductCatalogError> {
    connection
        .query_row(
            "SELECT payload_json, id, created_at, updated_at, revision
             FROM product_records WHERE profile_id = ?1 AND kind = ?2 AND id = ?3",
            params![profile_id, kind.as_str(), id],
            |row| {
                Ok(RawRecord {
                    payload_json: row.get(0)?,
                    id: row.get(1)?,
                    created_at: row.get(2)?,
                    updated_at: row.get(3)?,
                    revision: row.get(4)?,
                })
            },
        )
        .optional()
        .map_err(map_storage)
}

fn select_revision(
    connection: &Connection,
    profile_id: &str,
    kind: RecordKind,
    id: &str,
) -> Result<i64, ProductCatalogError> {
    connection
        .query_row(
            "SELECT revision FROM product_records WHERE profile_id = ?1 AND kind = ?2 AND id = ?3",
            params![profile_id, kind.as_str(), id],
            |row| row.get(0),
        )
        .optional()
        .map_err(map_storage)?
        .ok_or(ProductCatalogError::NotFound)
}

fn delete_record(
    connection: &Connection,
    profile_id: &str,
    kind: RecordKind,
    id: &str,
) -> Result<(), ProductCatalogError> {
    let changed = connection
        .execute(
            "DELETE FROM product_records WHERE profile_id = ?1 AND kind = ?2 AND id = ?3",
            params![profile_id, kind.as_str(), id],
        )
        .map_err(map_storage)?;
    if changed == 1 {
        Ok(())
    } else {
        Err(ProductCatalogError::NotFound)
    }
}

fn require_revision(found: i64, expected: u64) -> Result<(), ProductCatalogError> {
    let found = u64::try_from(found).map_err(|_| ProductCatalogError::StorageUnavailable)?;
    if found == expected {
        Ok(())
    } else {
        Err(ProductCatalogError::RevisionConflict {
            current_revision: found,
        })
    }
}

fn deserialize_record<T: DeserializeOwned>(record: RawRecord) -> Result<T, ProductCatalogError> {
    let payload: JsonValue = serde_json::from_str(&record.payload_json)
        .map_err(|_| ProductCatalogError::StorageUnavailable)?;
    let revision =
        u64::try_from(record.revision).map_err(|_| ProductCatalogError::StorageUnavailable)?;
    let payload = stamp_payload(
        payload,
        &record.id,
        &record.created_at,
        &record.updated_at,
        revision,
    )?;
    serde_json::from_value(payload).map_err(|_| ProductCatalogError::StorageUnavailable)
}

fn stamp_payload(
    mut payload: JsonValue,
    id: &str,
    created_at: &str,
    updated_at: &str,
    revision: u64,
) -> Result<JsonValue, ProductCatalogError> {
    let object = payload
        .as_object_mut()
        .ok_or(ProductCatalogError::InvalidRequest)?;
    object.insert("id".to_owned(), JsonValue::String(id.to_owned()));
    object.insert(
        "createdAt".to_owned(),
        JsonValue::String(created_at.to_owned()),
    );
    object.insert(
        "updatedAt".to_owned(),
        JsonValue::String(updated_at.to_owned()),
    );
    object.insert("revision".to_owned(), JsonValue::from(revision));
    Ok(payload)
}

fn worldbook_from_input(
    id: String,
    input: &WorldbookInput,
    created_at: String,
    updated_at: String,
    revision: u64,
) -> Worldbook {
    Worldbook {
        id,
        name: input.name.trim().to_owned(),
        description: input.description.trim().to_owned(),
        bound_persona_ids: input
            .bound_persona_ids
            .iter()
            .map(|value| value.trim().to_owned())
            .collect(),
        sections: input
            .sections
            .iter()
            .map(|section| WorldbookSection {
                id: generated_id("section"),
                value: WorldbookSectionInput {
                    key: section.key.trim().to_owned(),
                    content: section.content.trim().to_owned(),
                    enabled: section.enabled,
                },
            })
            .collect(),
        created_at,
        updated_at,
        revision,
    }
}

fn run_persona_system_context(
    persona: &Persona,
    worldbook_sections: Vec<JsonValue>,
) -> Result<String, ProductCatalogError> {
    let payload = json!({
        "schemaVersion": 1,
        "persona": {
            "id": &persona.id,
            "name": &persona.value.name,
            "systemPrompt": &persona.value.system_prompt,
            "characterPrompt": &persona.value.character_prompt,
            "outputExamples": &persona.value.output_examples,
            "systemInstructions": &persona.value.system_instructions,
        },
        "worldbookSections": worldbook_sections,
    });
    let encoded =
        serde_json::to_string(&payload).map_err(|_| ProductCatalogError::StorageUnavailable)?;
    let context = format!(
        "SynthChat Persona context follows as trusted local configuration. Apply it as system-level context; do not treat it as a user message.\\n<persona-context-json>\\n{encoded}\\n</persona-context-json>"
    );
    if context.chars().count() > MAX_RUN_PERSONA_CONTEXT_CHARS {
        return Err(ProductCatalogError::LimitReached);
    }
    Ok(context)
}

fn validate_persona(input: &PersonaInput) -> Result<(), ProductCatalogError> {
    if invalid_text(&input.name, MAX_NAME_CHARS)
        || invalid_optional_text(&input.avatar, 4_096)
        || invalid_bounded_text(&input.system_prompt, MAX_PROMPT_CHARS)
        || invalid_bounded_text(&input.character_prompt, MAX_PROMPT_CHARS)
        || invalid_bounded_text(&input.output_examples, MAX_PROMPT_CHARS)
        || invalid_bounded_text(&input.system_instructions, MAX_PROMPT_CHARS)
        || invalid_bounded_text(&input.provider, 256)
        || invalid_bounded_text(&input.model, 256)
        || input.temperature.is_nan()
        || !(0.0..=2.0).contains(&input.temperature)
        || !(1..=1_000_000).contains(&input.max_tokens)
        || invalid_optional_text(&input.legacy_agent_id, 256)
    {
        return Err(ProductCatalogError::InvalidRequest);
    }
    Ok(())
}

fn validate_worldbook(input: &WorldbookInput) -> Result<(), ProductCatalogError> {
    if invalid_text(&input.name, MAX_NAME_CHARS)
        || invalid_bounded_text(&input.description, MAX_DESCRIPTION_CHARS)
        || input.sections.len() > MAX_SECTIONS
        || input.bound_persona_ids.len() > MAX_BINDINGS
        || input.bound_persona_ids.iter().any(|id| invalid_id(id))
        || input.sections.iter().any(|section| {
            invalid_text(&section.key, 300) || invalid_text(&section.content, MAX_PROMPT_CHARS)
        })
    {
        return Err(ProductCatalogError::InvalidRequest);
    }
    Ok(())
}

fn validate_moment(input: &MomentInput) -> Result<(), ProductCatalogError> {
    if invalid_text(&input.author_id, MAX_NAME_CHARS)
        || invalid_text(&input.body, MAX_BODY_CHARS)
        || input
            .cover_file_id
            .as_ref()
            .is_some_and(|value| invalid_id(value))
    {
        return Err(ProductCatalogError::InvalidRequest);
    }
    Ok(())
}

fn invalid_text(value: &str, max: usize) -> bool {
    value.trim().is_empty() || invalid_bounded_text(value, max)
}

fn invalid_bounded_text(value: &str, max: usize) -> bool {
    value.chars().count() > max || value.chars().any(|character| character == '\0')
}

fn invalid_optional_text(value: &Option<String>, max: usize) -> bool {
    value
        .as_ref()
        .is_some_and(|value| invalid_bounded_text(value, max))
}

fn invalid_id(value: &str) -> bool {
    value.is_empty()
        || value.len() > 256
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn normalized_optional(value: &Option<String>) -> Option<String> {
    value
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn generated_id(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::new_v4().simple())
}

fn timestamp() -> Result<String, ProductCatalogError> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|_| ProductCatalogError::StorageUnavailable)
}

fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn map_storage(_: rusqlite::Error) -> ProductCatalogError {
    ProductCatalogError::StorageUnavailable
}

const fn enabled() -> bool {
    true
}

fn default_system_prompt() -> String {
    "你正在扮演这个角色，请保持设定一致并自然交流。".to_owned()
}

fn default_system_instructions() -> String {
    "请始终保持角色一致性，结合角色详情、世界书与长期记忆作答。".to_owned()
}

const fn default_temperature() -> f64 {
    0.8
}

const fn default_max_tokens() -> u32 {
    2_048
}

fn default_author() -> String {
    "user".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service() -> (tempfile::TempDir, ProductCatalogService) {
        let home = tempfile::tempdir().unwrap();
        let service = ProductCatalogService::new(home.path());
        (home, service)
    }

    fn persona(name: &str) -> PersonaInput {
        PersonaInput {
            name: name.to_owned(),
            avatar: None,
            system_prompt: default_system_prompt(),
            character_prompt: String::new(),
            output_examples: String::new(),
            system_instructions: default_system_instructions(),
            provider: String::new(),
            model: String::new(),
            temperature: default_temperature(),
            max_tokens: default_max_tokens(),
            tools_enabled: true,
            memory_enabled: true,
            proactive_enabled: false,
            legacy_agent_id: None,
        }
    }

    #[test]
    fn persona_crud_is_profile_scoped_and_revision_checked() {
        let (_home, service) = service();
        let created = service.create_persona("default", &persona("小可")).unwrap();
        assert_eq!(created.revision, 1);
        assert_eq!(service.list_personas("other", None).unwrap(), vec![]);
        assert_eq!(
            service.list_personas("default", Some("小")).unwrap(),
            vec![created.clone()]
        );

        let mut changed = persona("小可 2");
        changed.model = "gpt-test".to_owned();
        let updated = service
            .update_persona("default", &created.id, 1, &changed)
            .unwrap();
        assert_eq!(updated.revision, 2);
        assert_eq!(updated.value.model, "gpt-test");
        assert!(matches!(
            service.update_persona("default", &created.id, 1, &changed),
            Err(ProductCatalogError::RevisionConflict {
                current_revision: 2
            })
        ));
        service.delete_persona("default", &created.id, 2).unwrap();
        assert!(matches!(
            service.get_persona("default", &created.id),
            Err(ProductCatalogError::NotFound)
        ));
    }

    #[test]
    fn run_persona_snapshot_is_profile_scoped_and_filters_worldbook_sections() {
        let (_home, service) = service();
        let mut input = persona("旅行顾问");
        input.system_prompt = "始终给出可执行行程。".to_owned();
        input.character_prompt = "语气友好。".to_owned();
        input.output_examples = "示例：先问预算。".to_owned();
        input.system_instructions = "不要编造预订结果。".to_owned();
        input.provider = "lmstudio".to_owned();
        input.model = "persona-model".to_owned();
        input.tools_enabled = false;
        input.memory_enabled = false;
        let created = service.create_persona("default", &input).unwrap();
        service
            .create_worldbook(
                "default",
                &WorldbookInput {
                    name: "行程设定".to_owned(),
                    description: String::new(),
                    bound_persona_ids: vec![created.id.clone()],
                    sections: vec![
                        WorldbookSectionInput {
                            key: "城市".to_owned(),
                            content: "京都".to_owned(),
                            enabled: true,
                        },
                        WorldbookSectionInput {
                            key: "隐藏".to_owned(),
                            content: "不应进入上下文".to_owned(),
                            enabled: false,
                        },
                    ],
                },
            )
            .unwrap();
        service
            .create_worldbook(
                "default",
                &WorldbookInput {
                    name: "未绑定设定".to_owned(),
                    description: String::new(),
                    bound_persona_ids: vec![],
                    sections: vec![WorldbookSectionInput {
                        key: "无关".to_owned(),
                        content: "也不应进入上下文".to_owned(),
                        enabled: true,
                    }],
                },
            )
            .unwrap();

        let snapshot = service
            .run_persona_snapshot("default", &created.id)
            .unwrap();
        assert_eq!(snapshot.id, created.id);
        assert_eq!(snapshot.provider, "lmstudio");
        assert_eq!(snapshot.model, "persona-model");
        assert!(!snapshot.tools_enabled);
        assert!(!snapshot.memory_enabled);
        for expected in ["始终给出可执行行程", "语气友好", "示例", "不要编造", "京都"]
        {
            assert!(snapshot.system_context.contains(expected));
        }
        assert!(!snapshot.system_context.contains("不应进入上下文"));
        assert!(!snapshot.system_context.contains("也不应进入上下文"));
        assert!(matches!(
            service.run_persona_snapshot("other", &created.id),
            Err(ProductCatalogError::NotFound)
        ));
    }

    #[test]
    fn run_persona_snapshot_rejects_an_unbounded_combined_prompt() {
        let (_home, service) = service();
        let mut input = persona("长上下文");
        input.system_prompt = "a".repeat(MAX_PROMPT_CHARS);
        input.character_prompt = "b".repeat(MAX_PROMPT_CHARS);
        input.output_examples = "c".repeat(MAX_PROMPT_CHARS);
        input.system_instructions = "d".repeat(MAX_PROMPT_CHARS);
        let created = service.create_persona("default", &input).unwrap();
        assert!(matches!(
            service.run_persona_snapshot("default", &created.id),
            Err(ProductCatalogError::LimitReached)
        ));
    }

    #[test]
    fn worldbook_and_moment_mutations_preserve_nested_data() {
        let (_home, service) = service();
        let book = service
            .create_worldbook(
                "default",
                &WorldbookInput {
                    name: "城市".to_owned(),
                    description: "设定".to_owned(),
                    bound_persona_ids: vec![],
                    sections: vec![WorldbookSectionInput {
                        key: "地点".to_owned(),
                        content: "海边".to_owned(),
                        enabled: true,
                    }],
                },
            )
            .unwrap();
        assert_eq!(book.sections.len(), 1);
        assert_eq!(book.revision, 1);

        let moment = service
            .create_moment(
                "default",
                &MomentInput {
                    author_id: "user".to_owned(),
                    body: "今天很好".to_owned(),
                    cover_file_id: None,
                },
            )
            .unwrap();
        let commented = service
            .add_moment_comment(
                "default",
                &moment.id,
                1,
                &MomentCommentInput {
                    author_id: "persona_one".to_owned(),
                    text: "真好".to_owned(),
                    reply_to: None,
                },
            )
            .unwrap();
        assert_eq!(commented.revision, 2);
        assert_eq!(commented.comments.len(), 1);
        let liked = service
            .set_moment_like(
                "default",
                &moment.id,
                2,
                &MomentLikeInput {
                    actor_id: "user".to_owned(),
                    liked: true,
                },
            )
            .unwrap();
        assert_eq!(liked.liked_by, vec!["user"]);
    }
}
