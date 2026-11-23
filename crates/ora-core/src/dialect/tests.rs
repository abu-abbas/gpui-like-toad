//! The PostgreSQL and MySQL dialects, and type moves between all engines.

use super::*;
use crate::filter::{self, TableFilter, TablePage};
use crate::model::Value;
use crate::transfer::sqlgen::{self, keyset_select_sql, map_type, ColumnPlan, Resume};

fn plan(name: &str, ty: &str) -> ColumnPlan {
    ColumnPlan {
        source: name.into(),
        target: name.into(),
        target_type: ty.into(),
        nullable: true,
        default: None,
    }
}

#[test]
fn postgres_selects_ctid_text_and_pages_with_limit_offset() {
    let q = filter::select_sql(
        &PostgresDialect,
        "public",
        "emp",
        &TableFilter::default(),
        TablePage::new(100, 50).with_rowid(true),
    );
    assert_eq!(
        q.sql,
        "SELECT t.ctid::text AS \"__ROWID__\", t.* FROM \"public\".\"emp\" t LIMIT 50 OFFSET 100"
    );
    assert_eq!(
        PostgresDialect.row_id_match(2),
        "ctid = CAST(CAST(:2 AS text) AS tid)"
    );
}

#[test]
fn mysql_quotes_pages_and_has_no_row_id() {
    let q = filter::select_sql(
        &MysqlDialect,
        "shop",
        "o`rders",
        &TableFilter::default(),
        TablePage::new(0, 10),
    );
    assert_eq!(q.sql, "SELECT * FROM `shop`.`o``rders` LIMIT 10");
    assert!(!MysqlDialect.capabilities().row_id);
}

#[test]
fn keyset_paging_orders_by_the_sortable_key() {
    let cols = [plan("a", "integer")];
    let sql = keyset_select_sql(
        &PostgresDialect,
        "public",
        "t",
        &cols,
        None,
        &Resume::After(Value::Text("(10,3)".into())),
        100,
    )
    .unwrap();
    assert_eq!(
        sql,
        "SELECT t.ctid::text AS \"__KEY__\", t.\"a\" FROM \"public\".\"t\" t \
         WHERE t.ctid > '(10,3)'::tid ORDER BY t.ctid LIMIT 100"
    );
    // The value ends up in SQL text, so anything else is refused.
    for bad in ["(1,2); DROP", "(1)", "", "(a,b)"] {
        assert!(
            keyset_select_sql(
                &PostgresDialect,
                "public",
                "t",
                &cols,
                None,
                &Resume::After(Value::Text(bad.into())),
                1
            )
            .is_err(),
            "{bad}"
        );
    }
    assert!(keyset_select_sql(
        &MysqlDialect,
        "db",
        "t",
        &cols,
        None,
        &Resume::After(Value::Integer(1)),
        1
    )
    .is_err());
}

#[test]
fn types_go_through_the_neutral_list() {
    use DbKind::*;
    assert_eq!(map_type(Oracle, Postgres, "NUMBER(10)"), "BIGINT");
    assert_eq!(map_type(Oracle, Postgres, "NUMBER(4,0)"), "SMALLINT");
    assert_eq!(map_type(Oracle, Postgres, "NUMBER(8,2)"), "NUMERIC(8,2)");
    assert_eq!(map_type(Oracle, Postgres, "VARCHAR2(20)"), "VARCHAR(20)");
    assert_eq!(map_type(Oracle, Postgres, "CLOB"), "TEXT");
    assert_eq!(map_type(Oracle, Postgres, "DATE"), "TIMESTAMP");
    assert_eq!(map_type(Oracle, Postgres, "BLOB"), "BYTEA");
    assert_eq!(map_type(Oracle, Mysql, "VARCHAR2(20)"), "VARCHAR(20)");
    assert_eq!(map_type(Oracle, Mysql, "NUMBER(8,2)"), "DECIMAL(8,2)");
    assert_eq!(map_type(Oracle, Mysql, "DATE"), "DATETIME(6)");
    assert_eq!(map_type(Postgres, Oracle, "integer"), "NUMBER(10)");
    assert_eq!(map_type(Postgres, Oracle, "bigint"), "NUMBER(19)");
    assert_eq!(map_type(Postgres, Oracle, "text"), "CLOB");
    assert_eq!(
        map_type(Postgres, Oracle, "character varying(30)"),
        "VARCHAR2(30)"
    );
    assert_eq!(
        map_type(Postgres, Oracle, "timestamp with time zone"),
        "TIMESTAMP(6) WITH TIME ZONE"
    );
    assert_eq!(map_type(Postgres, Oracle, "boolean"), "NUMBER(1)");
    assert_eq!(map_type(Postgres, Mysql, "jsonb"), "JSON");
    assert_eq!(map_type(Postgres, Sqlite, "numeric(5,2)"), "NUMERIC");
    assert_eq!(map_type(Mysql, Postgres, "tinyint(1)"), "BOOLEAN");
    // A 32-bit integer is ten digits wide, so it widens to BIGINT (safe).
    assert_eq!(map_type(Mysql, Postgres, "int"), "BIGINT");
    assert_eq!(map_type(Mysql, Postgres, "longtext"), "TEXT");
    assert_eq!(map_type(Mysql, Postgres, "datetime"), "TIMESTAMP");
    assert_eq!(map_type(Mysql, Postgres, "blob"), "BYTEA");
    assert_eq!(map_type(Mysql, Oracle, "varchar(40)"), "VARCHAR2(40)");
    assert_eq!(map_type(Mysql, Sqlite, "double"), "REAL");
    // Same engine: the text stays as it is.
    assert_eq!(map_type(Postgres, Postgres, " citext "), "citext");
}

#[test]
fn native_types_settle_after_one_round_trip() {
    // A type an engine writes is read back by the same engine as a type
    // that writes the same text again (widening once is allowed).
    let types = [
        CanonType::Integer { digits: Some(10) },
        CanonType::Decimal {
            precision: Some(8),
            scale: Some(2),
        },
        CanonType::Double,
        CanonType::Text { len: Some(30) },
        CanonType::Clob,
        CanonType::Blob,
        CanonType::Timestamp,
        CanonType::Boolean,
    ];
    for kind in [DbKind::Oracle, DbKind::Postgres, DbKind::Mysql] {
        let d = kind.dialect();
        for t in &types {
            let once = d.native_type(&d.canonical_type(&d.native_type(t)));
            let twice = d.native_type(&d.canonical_type(&once));
            assert_eq!(once, twice, "{kind:?} {t:?}");
        }
    }
}

#[test]
fn literals_follow_each_engine() {
    let text = |k: DbKind, ty: &str, s: &str| k.dialect().text_literal(ty, s).unwrap();
    // A boolean from another engine fills a numeric column as 1 / 0.
    assert_eq!(text(DbKind::Sqlite, "INTEGER", "true"), "1");
    assert_eq!(text(DbKind::Oracle, "NUMBER", "false"), "0");
    assert_eq!(text(DbKind::Mysql, "tinyint(1)", "t"), "1");
    assert_eq!(text(DbKind::Postgres, "boolean", "true"), "'true'");
    assert_eq!(text(DbKind::Postgres, "integer", " 42 "), "42");
    assert_eq!(text(DbKind::Postgres, "text", "it's"), "'it''s'");
    assert_eq!(text(DbKind::Postgres, "date", "2024-01-31"), "'2024-01-31'");
    assert_eq!(text(DbKind::Mysql, "varchar(9)", "a\\b'c"), "'a\\\\b''c'");
    assert_eq!(text(DbKind::Mysql, "decimal(8,2)", "1.50"), "1.50");
    assert!(DbKind::Postgres
        .dialect()
        .text_literal("text", "a\0b")
        .is_err());
    assert_eq!(
        sqlgen::literal(DbKind::Postgres, "bytea", &Value::Bytes(vec![0xde, 0xad])).unwrap(),
        "'\\xDEAD'::bytea"
    );
    assert_eq!(
        sqlgen::literal(DbKind::Mysql, "blob", &Value::Bytes(vec![1, 2])).unwrap(),
        "X'0102'"
    );
}

#[test]
fn tables_indexes_and_keys_follow_each_engine() {
    let cols = [plan("id", "BIGINT"), plan("name", "VARCHAR(5)")];
    assert_eq!(
        sqlgen::create_table_sql(&MysqlDialect, "db", "t", &cols).unwrap(),
        "CREATE TABLE `db`.`t` (\n  `id` BIGINT,\n  `name` VARCHAR(5)\n)"
    );
    let idx = crate::model::IndexDef {
        name: "i1".into(),
        unique: true,
        index_type: "NORMAL".into(),
        columns: vec!["id".into()],
        status: "VALID".into(),
    };
    assert_eq!(
        sqlgen::create_index_sql(&PostgresDialect, "public", "t", &idx, &cols, None).unwrap(),
        "CREATE UNIQUE INDEX \"i1\" ON \"public\".\"t\" (\"id\")"
    );
    assert_eq!(
        sqlgen::create_index_sql(&MysqlDialect, "db", "t", &idx, &cols, None).unwrap(),
        "CREATE UNIQUE INDEX `i1` ON `db`.`t` (`id`)"
    );
    assert_eq!(
        crate::ddl::rename_table_sql(&MysqlDialect, "db", "a", "b").unwrap(),
        "RENAME TABLE `db`.`a` TO `db`.`b`"
    );
    assert_eq!(
        crate::ddl::rename_table_sql(&PostgresDialect, "public", "a", "b").unwrap(),
        "ALTER TABLE \"public\".\"a\" RENAME TO \"b\""
    );
    assert_eq!(MysqlDialect.default_clause("now()"), " DEFAULT (now())");
    assert_eq!(MysqlDialect.default_clause("'x'"), " DEFAULT 'x'");
}

#[test]
fn identity_defaults_are_auto_generated() {
    let col = |d: &str| crate::model::ColumnDef {
        name: "id".into(),
        data_type: "integer".into(),
        nullable: false,
        default: Some(d.into()),
    };
    assert!(PostgresDialect.is_auto_generated(&col("nextval('emp_id_seq'::regclass)")));
    assert!(!PostgresDialect.is_auto_generated(&col("0")));
    assert!(OracleDialect.is_auto_generated(&col("\"HR\".\"S\".NEXTVAL")));
}

#[test]
fn recordset_dates_use_each_engine_s_literal() {
    use crate::transfer::ranges::{generate, RangeKind};
    for (kind, lit) in [
        (DbKind::Postgres, "DATE '2024-01-01'"),
        (DbKind::Mysql, "DATE '2024-01-01'"),
    ] {
        let c = generate(kind, "d", RangeKind::Date, "2024-01-01", "2024-01-10", 1).unwrap();
        assert!(c[0].contains(lit), "{kind:?}: {}", c[0]);
    }
}
