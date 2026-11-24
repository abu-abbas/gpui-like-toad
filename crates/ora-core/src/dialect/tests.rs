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

#[test]
fn mysql_gives_keyed_text_columns_a_length() {
    assert_eq!(
        MysqlDialect.key_type("LONGTEXT").as_deref(),
        Some("VARCHAR(255)")
    );
    assert_eq!(
        MysqlDialect.key_type("longblob").as_deref(),
        Some("VARBINARY(255)")
    );
    assert_eq!(MysqlDialect.key_type("VARCHAR(20)"), None);
    assert_eq!(PostgresDialect.key_type("TEXT"), None);
}

#[test]
fn mssql_names_the_database_and_pages_with_an_order() {
    let target = filter::TableTarget::new(Some("staging"), "dbo", "users");
    let key = vec!["id".to_string(), "tenant]x".to_string()];
    let q = filter::select_sql_in(
        &MssqlDialect,
        &target,
        &TableFilter::default(),
        TablePage::new(50, 50),
        "NULL",
        &key,
    );
    assert_eq!(
        q.sql,
        "SELECT * FROM [staging].[dbo].[users] ORDER BY [id], [tenant]]x] \
         OFFSET 50 ROWS FETCH NEXT 50 ROWS ONLY"
    );
    // No key: still valid T-SQL, with no promised order.
    let q = filter::select_sql_in(
        &MssqlDialect,
        &target,
        &TableFilter::default(),
        TablePage::new(0, 10),
        "NULL",
        &[],
    );
    assert!(q
        .sql
        .ends_with("ORDER BY (SELECT NULL) OFFSET 0 ROWS FETCH NEXT 10 ROWS ONLY"));
    // The count names the same database.
    let c = filter::count_sql_in(&MssqlDialect, &target, &TableFilter::default());
    assert_eq!(c.sql, "SELECT COUNT(*) FROM [staging].[dbo].[users]");
    // Another database is another target.
    assert_eq!(
        MssqlDialect.table_ref_in(Some("dev"), "dbo", "users"),
        "[dev].[dbo].[users]"
    );
    assert_eq!(
        MssqlDialect.table_ref_in(None, "dbo", "users"),
        "[dbo].[users]"
    );
}

#[test]
fn the_other_engines_ignore_a_database_and_keep_their_paging() {
    for dialect in [
        &OracleDialect as &dyn Dialect,
        &SqliteDialect,
        &PostgresDialect,
        &MysqlDialect,
    ] {
        assert_eq!(
            dialect.table_ref_in(Some("x"), "s", "t"),
            dialect.table_ref("s", "t")
        );
        let (mut a, mut b) = (String::new(), String::new());
        dialect.push_page(&mut a, TablePage::new(10, 5));
        dialect.push_page_ordered(&mut b, TablePage::new(10, 5), &["id".to_string()]);
        assert_eq!(a, b);
        assert!(!dialect.capabilities().databases);
    }
}

#[test]
fn mssql_binds_become_at_p_and_brackets_are_left_alone() {
    let r = rewrite_binds(
        "SELECT [a:1], \"b\" FROM [t] WHERE x = :1 AND y LIKE :2 ESCAPE '\\'",
        &["1".to_string(), "%z%".to_string()],
        BindStyle::AtP,
    )
    .unwrap();
    assert_eq!(
        r.sql,
        "SELECT [a:1], \"b\" FROM [t] WHERE x = @P1 AND y LIKE @P2 ESCAPE '\\'"
    );
    assert_eq!(r.binds, ["1", "%z%"]);
}

#[test]
fn mssql_types_round_trip_through_the_neutral_ones() {
    let d = MssqlDialect;
    assert_eq!(d.canonical_type("bit"), CanonType::Boolean);
    assert_eq!(
        d.canonical_type("nvarchar(50)"),
        CanonType::Text { len: Some(50) }
    );
    assert_eq!(d.canonical_type("nvarchar(MAX)"), CanonType::Clob);
    assert_eq!(
        d.canonical_type("decimal(12,2)"),
        CanonType::Decimal {
            precision: Some(12),
            scale: Some(2)
        }
    );
    assert_eq!(d.native_type(&CanonType::Boolean), "BIT");
    assert_eq!(d.native_type(&CanonType::Timestamp), "DATETIME2");
    assert_eq!(d.text_literal("nvarchar(10)", "o'k").unwrap(), "N'o''k'");
    assert_eq!(d.bytes_literal(&[1, 255]).unwrap(), "0x01FF");
}
