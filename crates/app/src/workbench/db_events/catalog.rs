//! Responses that fill the explorer: schema and object lists.

use super::*;

impl Workbench {
    pub(super) fn on_schemas(&mut self, cid: u64, schemas: Vec<String>, cx: &mut Context<Self>) {
        let Some(conn) = self.conn_mut(cid) else {
            return;
        };
        // Keep the current pick on refresh; otherwise start in the
        // session user's own schema, as Toad does. A server with several
        // databases starts in the database the login opened.
        let databases = conn
            .last_connection
            .as_ref()
            .is_some_and(|l| l.profile.kind.capabilities().databases);
        let user = conn.session.as_ref().map(|s| {
            if databases {
                s.database.clone()
            } else {
                s.user.clone()
            }
        });
        let keep = conn.current_schema.clone().filter(|c| schemas.contains(c));
        conn.current_schema = keep
            .or_else(|| user.filter(|u| schemas.contains(u)))
            .or_else(|| schemas.first().cloned());
        conn.schemas = schemas.clone();
        self.with_catalog_for(cid, |catalog| catalog.set_schemas(schemas));
        self.load_objects_for(cid, cx);
    }

    pub(super) fn on_objects(
        &mut self,
        cid: u64,
        objects: Vec<SchemaObject>,
        cx: &mut Context<Self>,
    ) {
        let Some(conn) = self.conn_mut(cid) else {
            return;
        };
        let count = objects.len();
        conn.objects = objects;
        conn.selected_object = None;
        let schema = conn.current_schema.clone().unwrap_or_default();
        conn.status = format!("{schema}: loaded objects: {count}.");
        // Autocomplete metadata follows the explorer: (re)load the
        // schema just listed, and the session user's own schema,
        // which unqualified names resolve against.
        let user = conn.session.as_ref().map(|s| s.user.clone());
        // Not for a server with several databases: the login is no schema.
        let user = user.filter(|_| {
            !conn
                .last_connection
                .as_ref()
                .is_some_and(|l| l.profile.kind.capabilities().databases)
        });
        if !schema.is_empty() {
            self.load_catalog_for(cid, schema, cx);
        }
        if let Some(user) = user {
            let known = self
                .with_catalog_for(cid, |catalog| catalog.has_owner(&user))
                .unwrap_or(true);
            if !known {
                self.load_catalog_for(cid, user, cx);
            }
        }
    }
}
