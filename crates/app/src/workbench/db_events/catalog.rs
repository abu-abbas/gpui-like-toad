//! Responses that fill the explorer: schema and object lists.

use super::*;

impl Workbench {
    pub(super) fn on_schemas(&mut self, schemas: Vec<String>, cx: &mut Context<Self>) {
        let user = self.conn.session.as_ref().map(|s| s.user.clone());
        // Keep the current pick on refresh; otherwise start in the
        // session user's own schema, as Toad does.
        let keep = self
            .conn
            .current_schema
            .clone()
            .filter(|c| schemas.contains(c));
        self.conn.current_schema = keep
            .or_else(|| user.filter(|u| schemas.contains(u)))
            .or_else(|| schemas.first().cloned());
        self.catalog.borrow_mut().set_schemas(schemas.clone());
        self.conn.schemas = schemas;
        self.load_objects(cx);
    }

    pub(super) fn on_objects(&mut self, objects: Vec<SchemaObject>, cx: &mut Context<Self>) {
        let count = objects.len();
        self.conn.objects = objects;
        self.conn.selected_object = None;
        let schema = self.conn.current_schema.clone().unwrap_or_default();
        self.conn.status = format!("{schema}: loaded objects: {count}.");
        // Autocomplete metadata follows the explorer: (re)load the
        // schema just listed, and the session user's own schema,
        // which unqualified names resolve against.
        let user = self.conn.session.as_ref().map(|s| s.user.clone());
        if !schema.is_empty() {
            self.load_catalog(schema, cx);
        }
        if let Some(user) = user {
            if !self.catalog.borrow().has_owner(&user) {
                self.load_catalog(user, cx);
            }
        }
    }
}
