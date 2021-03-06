// Copyright 2018 Grove Enterprises LLC
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use arrow::datatypes::{Field, Schema};

use super::super::dfparser::{DFASTNode, DFParser};
use super::super::logicalplan::*;
use super::super::sqlplanner::{SchemaProvider, SqlToRel};
use super::datasource::DataSource;
use super::error::{ExecutionError, Result};
use super::expression::*;
use super::filter::FilterRelation;
use super::projection::ProjectRelation;
use super::relation::{DataSourceRelation, Relation};

pub struct ExecutionContext {
    datasources: Rc<RefCell<HashMap<String, Rc<RefCell<DataSource>>>>>,
}

impl ExecutionContext {
    pub fn new() -> Self {
        Self {
            datasources: Rc::new(RefCell::new(HashMap::new())),
        }
    }

    pub fn sql(&mut self, sql: &str) -> Result<Rc<RefCell<Relation>>> {
        let ast = DFParser::parse_sql(String::from(sql))?;

        match ast {
            //            DFASTNode::CreateExternalTable {
            //                name,
            //                columns,
            //                file_type,
            //                header_row,
            //                location,
            //            } => {
            //                let fields: Vec<Field> = columns
            //                    .iter()
            //                    .map(|c| Field::new(&c.name, convert_data_type(&c.data_type), c.allow_null))
            //                    .collect();
            //                let schema = Schema::new(fields);
            //
            //                let df = match file_type {
            //                    FileType::CSV => self.load_csv(&location, &schema, header_row, None)?,
            //                    FileType::NdJson => self.load_ndjson(&location, &schema, None)?,
            //                    FileType::Parquet => self.load_parquet(&location, None)?,
            //                };
            //
            //                self.register(&name, df);
            //
            //                //TODO: not sure what to return here
            //                Ok(Rc::new(DF::new(
            //                    self.clone(),
            //                    Rc::new(LogicalPlan::EmptyRelation {
            //                        schema: Rc::new(Schema::empty()),
            //                    }),
            //                )))
            //            }
            DFASTNode::ANSI(ansi) => {
                let schema_provider: Rc<SchemaProvider> = Rc::new(ExecutionContextSchemaProvider {
                    datasources: self.datasources.clone(),
                });

                // create a query planner
                let query_planner = SqlToRel::new(schema_provider);

                // plan the query (create a logical relational plan)
                let plan = query_planner.sql_to_rel(&ansi)?;
                //println!("Logical plan: {:?}", plan);

                let optimized_plan = plan; //push_down_projection(&plan, &HashSet::new());
                                           //println!("Optimized logical plan: {:?}", new_plan);

                let relation = self.execute(&optimized_plan)?;

                Ok(relation)
            }
            _ => unimplemented!(),
        }
    }

    pub fn register_datasource(&mut self, name: &str, ds: Rc<RefCell<DataSource>>) {
        self.datasources.borrow_mut().insert(name.to_string(), ds);
    }

    pub fn execute(&mut self, plan: &LogicalPlan) -> Result<Rc<RefCell<Relation>>> {
        println!("Logical plan: {:?}", plan);

        match *plan {
            //            LogicalPlan::EmptyRelation { .. } => Ok(Box::new(DataSourceRelation {
            //                schema: Schema::new(vec![]),
            //                ds: Rc::new(RefCell::new(EmptyRelation::new())),
            //            })),
            //
            //            LogicalPlan::Sort { .. } => unimplemented!(),
            LogicalPlan::TableScan { ref table_name, .. } => {
                match self.datasources.borrow().get(table_name) {
                    Some(ds) => {
                        //TODO: projection
                        Ok(Rc::new(RefCell::new(DataSourceRelation::new(ds.clone()))))
                    }
                    _ => Err(ExecutionError::General(format!(
                        "No table registered as '{}'",
                        table_name
                    ))),
                }
            }
            LogicalPlan::Selection {
                ref expr,
                ref input,
            } => {
                let input_rel = self.execute(input)?;
                let input_schema = input_rel.as_ref().borrow().schema().clone();
                let runtime_expr = compile_scalar_expr(&self, expr, &input_schema)?;
                let rel = FilterRelation::new(
                    input_rel,
                    runtime_expr, /*.get_func().clone()*/
                    input_schema,
                );
                Ok(Rc::new(RefCell::new(rel)))
            }
            LogicalPlan::Projection {
                ref expr,
                ref input,
                ..
            } => {
                let input_rel = self.execute(input)?;

                let input_schema = input_rel.as_ref().borrow().schema().clone();

                let project_columns: Vec<Field> = exprlist_to_fields(&expr, &input_schema);

                let project_schema = Arc::new(Schema::new(project_columns));

                let compiled_expr: Result<Vec<RuntimeExpr>> = expr
                    .iter()
                    .map(|e| compile_scalar_expr(&self, e, &input_schema))
                    .collect();

                let rel = ProjectRelation::new(input_rel, compiled_expr?, project_schema);

                Ok(Rc::new(RefCell::new(rel)))
            }
            _ => unimplemented!(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum ExecutionResult {
    Unit,
    Count(usize),
    Str(String),
}

pub fn expr_to_field(e: &Expr, input_schema: &Schema) -> Field {
    match e {
        Expr::Column(i) => input_schema.fields()[*i].clone(),
        Expr::Literal(ref lit) => Field::new("lit", lit.get_datatype(), true),
        Expr::ScalarFunction {
            ref name,
            ref return_type,
            ..
        } => Field::new(&name, return_type.clone(), true),
        Expr::AggregateFunction {
            ref name,
            ref return_type,
            ..
        } => Field::new(&name, return_type.clone(), true),
        Expr::Cast { ref data_type, .. } => Field::new("cast", data_type.clone(), true),
        Expr::BinaryExpr {
            ref left,
            ref right,
            ..
        } => {
            let left_type = left.get_type(input_schema);
            let right_type = right.get_type(input_schema);
            Field::new(
                "binary_expr",
                get_supertype(&left_type, &right_type).unwrap(),
                true,
            )
        }
        _ => unimplemented!("Cannot determine schema type for expression {:?}", e),
    }
}

pub fn exprlist_to_fields(expr: &Vec<Expr>, input_schema: &Schema) -> Vec<Field> {
    expr.iter()
        .map(|e| expr_to_field(e, input_schema))
        .collect()
}

struct ExecutionContextSchemaProvider {
    datasources: Rc<RefCell<HashMap<String, Rc<RefCell<DataSource>>>>>,
}
impl SchemaProvider for ExecutionContextSchemaProvider {
    fn get_table_meta(&self, name: &str) -> Option<Arc<Schema>> {
        match self.datasources.borrow().get(name) {
            Some(ds) => Some(ds.borrow().schema().clone()),
            None => None,
        }
    }

    fn get_function_meta(&self, _name: &str) -> Option<Arc<FunctionMeta>> {
        unimplemented!()
    }
}
