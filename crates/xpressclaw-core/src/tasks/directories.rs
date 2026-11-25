//! Directories an Agent explicitly registers for a task. Paths and Git commands
//! belong to that Agent's container, which can see its worktrees and mounts.
use std::collections::BTreeMap;
use std::future::Future;
use std::time::Duration;

use futures_util::{stream, StreamExt};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    db::Database,
    docker::manager::DockerManager,
    error::{Error, Result},
};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ChangedFile {
    pub path: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<GitVersion>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct GitVersion {
    pub common: String,
    pub object: String,
}

#[derive(Debug)]
pub struct Directory {
    pub id: String,
    pub path: String,
    pub files: Vec<ChangedFile>,
    pub notice: Option<String>,
}

impl Directory {
    pub fn view(&self) -> Value {
        let files: Vec<_> = self
            .files
            .iter()
            .map(|file| {
                json!({
                    "path": file.path, "status": file.status, "has_version": file.version.is_some()
                })
            })
            .collect();
        json!({"id":self.id, "path":self.path, "files":files, "notice":self.notice})
    }
}

pub fn list(db: &Database, task: &str, agent: &str) -> Result<Vec<Directory>> {
    db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT d.id, d.path, d.files, d.notice FROM task_directories d
             JOIN tasks t ON t.id=d.task_id AND t.agent_id=d.agent_id
             WHERE d.task_id=?1 AND d.agent_id=?2 ORDER BY d.path",
        )?;
        let mut rows = stmt.query(params![task, agent])?;
        let mut directories = vec![];
        while let Some(row) = rows.next()? {
            directories.push(Directory {
                id: row.get(0)?,
                path: row.get(1)?,
                files: serde_json::from_str(&row.get::<_, String>(2)?)?,
                notice: row.get(3)?,
            });
        }
        Ok(directories)
    })
}

fn running_attempt(db: &Database, task: &str, agent: &str) -> Result<String> {
    db.with_conn(|conn| {
        conn.query_row(
            "SELECT a.id FROM work_attempts a JOIN tasks t ON t.id=a.task_id AND t.agent_id=a.session_id
             WHERE a.task_id=?1 AND a.session_id=?2 AND a.status='running' LIMIT 1",
            params![task, agent], |row| row.get(0)
        ).optional()?.ok_or_else(|| Error::Task("Register a directory from the assigned task's running turn".into()))
    })
}

#[derive(Deserialize)]
struct Baseline {
    path: String,
    baseline: Value,
    omitted: Vec<String>,
}

fn omitted_notice(omitted: &[String]) -> Option<String> {
    (!omitted.is_empty()).then(|| {
        format!(
            "Large untracked directories omitted: {}",
            omitted.join(", ")
        )
    })
}

async fn baseline(docker: &DockerManager, agent: &str, path: &str) -> Result<Baseline> {
    let value = docker
        .container_files(
            agent,
            json!({"operation":"directory_baseline", "path":path}),
        )
        .await?;
    Ok(serde_json::from_value(value)?)
}

pub async fn register(
    db: &Database,
    docker: &DockerManager,
    task: &str,
    agent: &str,
    path: &str,
) -> Result<Value> {
    let attempt = running_attempt(db, task, agent)?;
    if !path.starts_with('/') || path.len() > 4096 || path.contains('\0') {
        return Err(Error::Task(
            "An absolute container directory is required".into(),
        ));
    }
    // Normal MCP calls already pass a canonical path. A retry must preserve
    // the original baseline, even if the directory has since become dirty.
    let existing: Option<String> = db.with_conn(|conn| conn.query_row(
        "SELECT id FROM task_directories WHERE task_id=?1 AND agent_id=?2 AND path=?3 AND attempt_id=?4 AND baseline IS NOT NULL",
        params![task, agent, path, attempt], |row| row.get(0)).optional())?;
    if let Some(id) = existing {
        return Ok(json!({"id":id, "path":path}));
    }
    let captured = baseline(docker, agent, path).await?;
    save_registration(db, task, agent, &attempt, captured)
}

fn save_registration(
    db: &Database,
    task: &str,
    agent: &str,
    attempt: &str,
    capture: Baseline,
) -> Result<Value> {
    let baseline = serde_json::to_string(&capture.baseline)?;
    db.with_conn(|conn| {
        let tx = conn.unchecked_transaction()?;
        let running: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM work_attempts a JOIN tasks t ON t.id=a.task_id AND t.agent_id=a.session_id
             WHERE a.id=?1 AND a.task_id=?2 AND a.session_id=?3 AND a.status='running')",
            params![attempt, task, agent], |row| row.get(0))?;
        if !running { return Err(Error::Task("The registering turn has ended".into())); }
        let existing: Option<String> = tx.query_row("SELECT id FROM task_directories WHERE task_id=?1 AND agent_id=?2 AND path=?3",
            params![task, agent, capture.path], |row| row.get(0)).optional()?;
        let notice = omitted_notice(&capture.omitted);
        let id = if let Some(id) = existing {
            tx.execute("UPDATE task_directories SET baseline=?1, previous_files=files, attempt_id=?2, notice=?3
                        WHERE id=?4 AND (attempt_id!=?2 OR baseline IS NULL)", params![baseline, attempt, notice, id])?;
            id
        } else {
            let count: i64 = tx.query_row("SELECT count(*) FROM task_directories WHERE task_id=?1 AND agent_id=?2", params![task,agent], |r| r.get(0))?;
            if count >= 16 { return Err(Error::Task("A task can track up to 16 directories".into())); }
            let id = uuid::Uuid::new_v4().to_string();
            tx.execute("INSERT INTO task_directories (id,task_id,agent_id,path,attempt_id,baseline,notice) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![id,task,agent,capture.path,attempt,baseline,notice])?;
            id
        };
        tx.commit()?;
        Ok(json!({"id":id, "path":capture.path, "notice":notice}))
    })
}

#[derive(Deserialize)]
struct Changes {
    files: Vec<ChangedFile>,
    omitted: Vec<String>,
}

fn save_changes(
    db: &Database,
    id: &str,
    attempt: &str,
    changes: Changes,
    finished: bool,
) -> Result<()> {
    db.with_conn(|conn| {
        let prior: Option<String> = conn.query_row(
            "SELECT previous_files FROM task_directories WHERE id=?1 AND attempt_id=?2 AND baseline IS NOT NULL",
            params![id, attempt], |row| row.get(0)).optional()?;
        let Some(prior) = prior else { return Ok(()) };
        let mut files = BTreeMap::new();
        for file in serde_json::from_str::<Vec<ChangedFile>>(&prior)?.into_iter().chain(changes.files) {
            files.insert(file.path.clone(), file);
        }
        conn.execute("UPDATE task_directories SET files=?1, notice=?2,
                      baseline=CASE WHEN ?3 THEN NULL ELSE baseline END WHERE id=?4 AND attempt_id=?5
                      AND (?3 OR files!=?1 OR notice IS NOT ?2)",
            params![serde_json::to_string(&files.into_values().collect::<Vec<_>>())?, omitted_notice(&changes.omitted),finished,id,attempt])?;
        Ok(())
    })
}

struct Turn<'a> {
    db: &'a Database,
    docker: &'a DockerManager,
    task: &'a str,
    agent: &'a str,
    attempt: &'a str,
}

impl Turn<'_> {
    async fn begin(&self) -> Result<()> {
        for directory in list(self.db, self.task, self.agent)? {
            let capture = baseline(self.docker, self.agent, &directory.path).await;
            let (base, notice) = match capture {
                Ok(capture) if capture.path == directory.path => (
                    Some(serde_json::to_string(&capture.baseline)?),
                    omitted_notice(&capture.omitted),
                ),
                Ok(_) => (None, Some("The registered directory has moved".into())),
                Err(error) => (None, Some(error.to_string())),
            };
            self.db.with_conn(|conn| conn.execute(
                "UPDATE task_directories SET attempt_id=?1, previous_files=files, baseline=?2, notice=?3
                 WHERE id=?4 AND attempt_id!=?1", params![self.attempt, base, notice, directory.id]))?;
        }
        Ok(())
    }

    async fn refresh(&self, finished: bool) -> Result<()> {
        let rows = self.db.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT id,path,baseline FROM task_directories WHERE task_id=?1 AND agent_id=?2 AND attempt_id=?3 AND baseline IS NOT NULL")?;
            let rows = stmt.query_map(params![self.task,self.agent,self.attempt], |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?)))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })?;
        stream::iter(rows).for_each_concurrent(4, |(id,path,baseline)| async move {
            let result: Result<()> = async {
                let request = json!({"operation":"directory_changes", "path":path, "baseline":serde_json::from_str::<Value>(&baseline)?});
                let response = self.docker.container_files(self.agent, request).await?;
                save_changes(self.db, &id, self.attempt, serde_json::from_value(response)?, finished)
            }.await;
            if let Err(error) = result {
                if let Err(db_error) = self.db.with_conn(|conn| conn.execute(
                    "UPDATE task_directories SET notice=?1, baseline=CASE WHEN ?2 THEN NULL ELSE baseline END WHERE id=?3 AND attempt_id=?4
                     AND (?2 OR notice IS NOT ?1)",
                    params![error.to_string(), finished, id, self.attempt])) {
                    tracing::warn!(%db_error, "could not record task directory error");
                }
            }
        }).await;
        Ok(())
    }
}

/// Sample independently of tool notifications, including when no browser is
/// watching. Finish the last sample before the worker releases the task lane.
pub async fn track<T>(
    db: &Database,
    docker: &DockerManager,
    task: &str,
    agent: &str,
    attempt: &str,
    work: impl Future<Output = T>,
) -> T {
    let turn = Turn {
        db,
        docker,
        task,
        agent,
        attempt,
    };
    if let Err(error) = turn.begin().await {
        tracing::warn!(%error, %task, "could not start task directory tracking");
    }
    let observe = async {
        loop {
            tokio::time::sleep(Duration::from_secs(8)).await;
            if let Err(error) = turn.refresh(false).await {
                tracing::warn!(%error, %task, "could not refresh task directories");
            }
        }
    };
    let result = tokio::select! { result = work => result, () = observe => unreachable!() };
    if let Err(error) = turn.refresh(true).await {
        tracing::warn!(%error, %task, "could not finalize task directories");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registration(path: &str, head: &str) -> Baseline {
        Baseline {
            path: path.to_string(),
            baseline: json!({"head":head}),
            omitted: vec![],
        }
    }

    fn changed(path: &str) -> Changes {
        Changes {
            files: vec![ChangedFile {
                path: path.to_string(),
                status: "M".into(),
                version: None,
            }],
            omitted: vec![],
        }
    }

    #[test]
    fn retries_and_followups_preserve_task_files_and_ignore_obsolete_observers() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("db");
        let db = Database::open(&path).unwrap();
        db.conn().execute_batch("
            INSERT INTO agents(id,name,backend,config) VALUES ('agent','A','native','{}'),('other','B','native','{}');
            INSERT INTO logical_sessions(id,agent_id) VALUES ('agent','agent');
            INSERT INTO tasks(id,title,status,agent_id) VALUES ('task','Code','in_progress','agent');
            INSERT INTO work_attempts(id,session_id,task_id,runner,status) VALUES ('first','agent','task','test','running');"
        ).unwrap();
        assert!(running_attempt(&db, "task", "other").is_err());
        assert_eq!(running_attempt(&db, "task", "agent").unwrap(), "first");
        let register = |head| {
            save_registration(
                &db,
                "task",
                "agent",
                "first",
                registration("/tmp/checkout", head),
            )
            .unwrap()
        };
        let id = register("base")["id"].as_str().unwrap().to_owned();
        assert_eq!(register("later")["id"], id);
        assert_eq!(
            db.conn()
                .query_row(
                    "SELECT json_extract(baseline,'$.head') FROM task_directories",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "base"
        );
        save_changes(&db, &id, "first", changed("transient.rs"), false).unwrap();
        save_changes(&db, &id, "first", changed("committed.rs"), true).unwrap();
        // No active browser is needed after the turn. Reopen from the saved data.
        drop(db);
        let db = Database::open(&path).unwrap();
        assert_eq!(
            list(&db, "task", "agent").unwrap()[0].files[0].path,
            "committed.rs"
        );
        assert!(list(&db, "task", "other").unwrap().is_empty());
        db.conn().execute_batch("UPDATE work_attempts SET status='completed';
            INSERT INTO work_attempts(id,session_id,task_id,runner,status) VALUES ('next','agent','task','test','running');").unwrap();
        assert!(save_registration(
            &db,
            "task",
            "agent",
            "first",
            registration("/tmp/late", "x")
        )
        .is_err());
        save_registration(
            &db,
            "task",
            "agent",
            "next",
            registration("/tmp/checkout", "next-base"),
        )
        .unwrap();
        save_changes(&db, &id, "first", changed("obsolete.rs"), true).unwrap();
        save_changes(&db, &id, "next", changed("followup.rs"), true).unwrap();
        let directories = list(&db, "task", "agent").unwrap();
        assert_eq!(
            directories[0]
                .files
                .iter()
                .map(|f| f.path.as_str())
                .collect::<Vec<_>>(),
            ["committed.rs", "followup.rs"]
        );
    }
}
