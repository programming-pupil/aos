//! JOIN path discovery — builds an FK graph and finds join paths between tables.
//!
//! For NL2SQL queries that span multiple tables, the LLM needs to know which tables
//! can be legitimately joined and through which intermediate tables. This module
//! computes all reachable join paths (up to `MAX_JOIN_HOPS`) between a given pair of
//! tables and stores them in `nl2sql_join_paths` for fast retrieval at query time.
//!
//! Algorithm: BFS on the FK graph from `source_table` until `target_table` is reached
//! or the frontier exceeds `MAX_JOIN_HOPS`. All paths of minimum length are returned.

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::collections::{HashMap, HashSet, VecDeque};

/// Maximum number of JOIN hops allowed in a discovered path.
/// Paths longer than this are not stored (prevents exponential blowup on large schemas).
const MAX_JOIN_HOPS: usize = 4;

/// A single step in a JOIN path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JoinStep {
    pub source_table: String,
    pub source_column: String,
    pub target_table: String,
    pub target_column: String,
}

/// A complete JOIN path between two tables.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinPath {
    /// Ordered list of FK edges forming the path.
    pub steps: Vec<JoinStep>,
    /// Total number of hops (steps.len()).
    pub hops: usize,
}

impl JoinPath {
    /// Human-readable description of the path, suitable for injecting into LLM prompts.
    pub fn to_prompt_text(&self) -> String {
        let legs: Vec<String> = self
            .steps
            .iter()
            .map(|s| {
                format!(
                    "{}.{} → {}.{}",
                    s.source_table, s.source_column, s.target_table, s.target_column
                )
            })
            .collect();
        format!(
            "{} ({} hop{})",
            legs.join(", "),
            self.hops,
            if self.hops == 1 { "" } else { "s" }
        )
    }

    /// Returns SQL JOIN clauses that can be pasted directly into a FROM clause.
    /// Assumes the source table is already in scope; generates `JOIN ... ON ...` for each step.
    pub fn to_sql_joins(&self, aliases: &HashMap<String, String>) -> String {
        self.steps
            .iter()
            .map(|s| {
                let src_alias = aliases
                    .get(&s.source_table)
                    .cloned()
                    .unwrap_or_else(|| s.source_table.clone());
                let tgt_alias = aliases
                    .get(&s.target_table)
                    .cloned()
                    .unwrap_or_else(|| s.target_table.clone());
                format!(
                    "INNER JOIN {} AS {} ON {}.{} = {}.{}",
                    s.target_table,
                    tgt_alias,
                    src_alias,
                    s.source_column,
                    tgt_alias,
                    s.target_column
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// An edge in the FK graph: source_table.col → target_table.col
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct FkEdge {
    source_table: String,
    source_column: String,
    target_table: String,
    target_column: String,
}

impl FkEdge {
    fn new(st: String, sc: String, tt: String, tc: String) -> Self {
        Self {
            source_table: st,
            source_column: sc,
            target_table: tt,
            target_column: tc,
        }
    }

    fn reverse(&self) -> FkEdge {
        FkEdge::new(
            self.target_table.clone(),
            self.target_column.clone(),
            self.source_table.clone(),
            self.source_column.clone(),
        )
    }
}

/// BFS state: current table + path of edges taken to reach it.
struct BfsNode {
    table: String,
    path: Vec<FkEdge>,
}

impl FkGraph {
    /// Build a graph from FK rows (table_name, column_name, target_table, target_column).
    fn from_fk_rows(rows: Vec<(String, String, String, String)>) -> Self {
        let mut forward: HashMap<String, Vec<FkEdge>> = HashMap::new();
        let mut reverse: HashMap<String, Vec<FkEdge>> = HashMap::new();

        for (st, sc, tt, tc) in rows {
            let edge = FkEdge::new(st.clone(), sc, tt.clone(), tc);
            forward.entry(st).or_default().push(edge.clone());
            reverse.entry(tt).or_default().push(edge.reverse());
        }

        FkGraph { forward, reverse }
    }

    /// BFS from `start` to `end` within `max_hops`.
    /// Returns all shortest paths found.
    fn find_paths(&self, start: &str, end: &str, max_hops: usize) -> Vec<JoinPath> {
        if start == end {
            return vec![JoinPath {
                steps: vec![],
                hops: 0,
            }];
        }

        let mut queue: VecDeque<BfsNode> = VecDeque::new();
        queue.push_back(BfsNode {
            table: start.to_owned(),
            path: vec![],
        });

        let mut visited_at_depth: HashMap<String, usize> = HashMap::new();
        visited_at_depth.insert(start.to_owned(), 0);

        let mut results: Vec<JoinPath> = Vec::new();
        let mut best_hops: Option<usize> = None;

        while let Some(BfsNode { table, path }) = queue.pop_front() {
            let depth = path.len();

            // If we've already found strictly shorter paths, skip this branch.
            if let Some(best) = best_hops {
                if depth >= best {
                    continue;
                }
            }

            // Stop expanding if we've hit max hops.
            if depth >= max_hops {
                continue;
            }

            // Explore forward edges (A.id → B.fk) and reverse edges (B.fk → A.id).
            let neighbors: Vec<_> = self
                .forward
                .get(&table)
                .iter()
                .chain(self.reverse.get(&table).iter())
                .flat_map(|v| v.iter())
                .filter(|e| {
                    let next_table = if e.source_table == table {
                        e.target_table.clone()
                    } else {
                        e.source_table.clone()
                    };
                    visited_at_depth
                        .get(&next_table)
                        .map(|&d| depth + 1 < d)
                        .unwrap_or(true)
                })
                .cloned()
                .collect();

            for edge in neighbors {
                let next_table = if edge.source_table == table {
                    edge.target_table.clone()
                } else {
                    edge.source_table.clone()
                };

                let mut new_path = path.clone();
                new_path.push(edge);

                if next_table == end {
                    let path_len = new_path.len();
                    best_hops = Some(path_len);
                    results.push(JoinPath {
                        steps: new_path
                            .into_iter()
                            .map(|e| JoinStep {
                                source_table: e.source_table,
                                source_column: e.source_column,
                                target_table: e.target_table,
                                target_column: e.target_column,
                            })
                            .collect(),
                        hops: path_len,
                    });
                } else {
                    visited_at_depth.insert(next_table.clone(), depth + 1);
                    queue.push_back(BfsNode {
                        table: next_table,
                        path: new_path,
                    });
                }
            }
        }

        results
    }
}

/// In-memory FK adjacency graph for a single datasource.
struct FkGraph {
    /// Outgoing edges from each table.
    forward: HashMap<String, Vec<FkEdge>>,
    /// Reverse lookup for reverse-join traversal.
    reverse: HashMap<String, Vec<FkEdge>>,
}

/// Load all FKs for a datasource from nl2sql_foreign_keys (user-defined + auto-detected).
async fn load_fk_rows(
    db: &SqlitePool,
    datasource_id: &str,
) -> Vec<(String, String, String, String)> {
    sqlx::query_as::<_, (String, String, String, String)>(
        "SELECT source_table, source_column, target_table, target_column \
         FROM nl2sql_foreign_keys \
         WHERE datasource_id = ? AND deleted_at IS NULL",
    )
    .bind(datasource_id)
    .fetch_all(db)
    .await
    .unwrap_or_default()
}

/// P1-3: Load cross-datasource edges for a specific datasource from nl2sql_cross_datasource_relations.
/// Returns edges as (source_table, source_column, target_table, target_column) for all relations
/// where this datasource appears on either side. These edges are treated as virtual within the graph
/// to enable cross-source JOIN path discovery.
async fn load_cross_ds_edges(
    db: &SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
) -> Vec<(String, String, String, String)> {
    #[derive(Debug, sqlx::FromRow)]
    struct CrossDsEdge {
        left_table: String,
        left_column: String,
        right_table: String,
        right_column: String,
    }
    #[derive(Debug, sqlx::FromRow)]
    struct CrossDsEdgeReversed {
        right_table: String,
        right_column: String,
        left_table: String,
        left_column: String,
    }

    // Edges where this datasource is the LEFT side (it → other datasource)
    let left_edges: Vec<CrossDsEdge> = sqlx::query_as(
        "SELECT left_table, left_column, right_table, right_column \
         FROM nl2sql_cross_datasource_relations \
         WHERE tenant_id = ? AND left_datasource_id = ? AND deleted_at IS NULL",
    )
    .bind(tenant_id)
    .bind(datasource_id)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    // Edges where this datasource is the RIGHT side (other datasource → it)
    let right_edges: Vec<CrossDsEdgeReversed> = sqlx::query_as(
        "SELECT left_table, left_column, right_table, right_column \
         FROM nl2sql_cross_datasource_relations \
         WHERE tenant_id = ? AND right_datasource_id = ? AND deleted_at IS NULL",
    )
    .bind(tenant_id)
    .bind(datasource_id)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let mut edges: Vec<(String, String, String, String)> = Vec::new();
    for e in left_edges {
        edges.push((e.left_table, e.left_column, e.right_table, e.right_column));
    }
    for e in right_edges {
        // P1-3 BUG-FIX: Reverse direction — "other → this" becomes "this → other".
        // Edges are (source_table, source_column, target_table, target_column).
        // When this datasource is on the RIGHT side, the direction must flip:
        // other.left_table.left_column → this.right_table.right_column
        // becomes: this.right_table.right_column → other.left_table.left_column.
        edges.push((e.right_table, e.right_column, e.left_table, e.left_column));
    }
    edges
}

/// Build the join path table by computing all-pairs shortest paths for tables in the schema.
/// Replaces existing rows for this datasource to ensure paths are fresh.
/// P1-3: Also loads cross-datasource edges from nl2sql_cross_datasource_relations and
/// integrates them into the FK graph to enable cross-source JOIN path discovery.
pub async fn rebuild_join_paths(
    db: &SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
) -> anyhow::Result<usize> {
    let rows = load_fk_rows(db, datasource_id).await;
    let cross_ds_rows = load_cross_ds_edges(db, tenant_id, datasource_id).await;

    // Merge within-datasource FKs and cross-datasource edges into a unified graph.
    let mut all_rows = rows;
    all_rows.extend(cross_ds_rows);

    if all_rows.is_empty() {
        return Ok(0);
    }

    let graph = FkGraph::from_fk_rows(all_rows);

    // Collect all unique tables involved in any FK.
    let mut all_tables: HashSet<String> = HashSet::new();
    for st in graph.forward.keys() {
        if let Some(edges) = graph.forward.get(st) {
            for edge in edges {
                all_tables.insert(st.clone());
                all_tables.insert(edge.target_table.clone());
            }
        }
    }

    // Compute all-pairs shortest paths for each ordered pair of distinct tables.
    // (source_table, target_table, source_column, target_column, path_text, sql_joins, hops)
    let mut paths_to_insert: Vec<(String, String, String, String, String, String, usize)> =
        Vec::new();
    let table_list: Vec<String> = all_tables.into_iter().collect();

    for i in 0..table_list.len() {
        for j in 0..table_list.len() {
            if i == j {
                continue;
            }
            let source = &table_list[i];
            let target = &table_list[j];

            let join_paths = graph.find_paths(source, target, MAX_JOIN_HOPS);
            for path in join_paths {
                let path_text = path.to_prompt_text();
                let _hops = path.hops as i32;
                let sql_joins = path.to_sql_joins(&HashMap::new());

                // Extract direct FK columns from the first hop of the path.
                let (source_column, target_column) = path
                    .steps
                    .first()
                    .map(|s| (s.source_column.clone(), s.target_column.clone()))
                    .unwrap_or_else(|| (String::new(), String::new()));

                paths_to_insert.push((
                    source.clone(),
                    target.clone(),
                    source_column,
                    target_column,
                    path_text,
                    sql_joins,
                    path.hops,
                ));
            }
        }
    }

    if paths_to_insert.is_empty() {
        return Ok(0);
    }

    // Refresh discovered paths in a transaction.
    // Preserve admin-curated manual paths (`source='manual'`) on rediscover.
    let mut tx = sqlx::Acquire::begin(db).await?;
    sqlx::query(
        "DELETE FROM nl2sql_join_paths \
         WHERE tenant_id = ? AND datasource_id = ? AND source <> 'manual'",
    )
    .bind(tenant_id)
    .bind(datasource_id)
    .execute(&mut *tx)
    .await?;

    let _path_count = paths_to_insert.len();
    for (source, target, source_col, target_col, path_text, sql_joins, hops) in &paths_to_insert {
        // Use INSERT IGNORE so auto-discovered rows won't clobber existing manual rows
        // that share the same unique key (datasource_id, source_table, target_table, hops).
        sqlx::query(
            "INSERT OR IGNORE INTO nl2sql_join_paths \
             (tenant_id, datasource_id, source_table, target_table, \
              source_column, target_column, path_text, sql_joins, hops, source) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'auto')",
        )
        .bind(tenant_id)
        .bind(datasource_id)
        .bind(source)
        .bind(target)
        .bind(source_col)
        .bind(target_col)
        .bind(path_text)
        .bind(sql_joins)
        .bind(*hops as i32)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    tracing::info!(
        datasource_id = %datasource_id,
        path_count = paths_to_insert.len(),
        "rebuilt join paths"
    );

    Ok(paths_to_insert.len())
}

/// Retrieve all stored join paths from `source_table` to `target_table` for a datasource.
/// Returns up to `limit` paths, sorted by fewest hops first.
pub async fn get_join_paths(
    db: &SqlitePool,
    datasource_id: &str,
    source_table: &str,
    target_table: &str,
    _limit: usize,
) -> Vec<(String, String, usize)> {
    sqlx::query_as::<_, (String, String, u16)>(
        "SELECT path_text, sql_joins, hops \
         FROM nl2sql_join_paths \
         WHERE datasource_id = ? AND source_table = ? AND target_table = ? \
         ORDER BY hops ASC \
         LIMIT ?",
    )
    .bind(datasource_id)
    .bind(source_table)
    .bind(target_table)
    .fetch_all(db)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|(pt, sj, h)| (pt, sj, usize::from(h)))
    .collect()
}

/// Discover and persist join paths for a datasource, called after schema refresh
/// or after FK discovery. Idempotent — safe to call on every refresh.
pub async fn refresh_join_paths(
    db: &SqlitePool,
    tenant_id: &str,
    datasource_id: &str,
) -> anyhow::Result<usize> {
    rebuild_join_paths(db, tenant_id, datasource_id).await
}
