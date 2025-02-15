// Copyright 2022 The Blaze Authors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::{
    any::Any,
    fmt::{Debug, Formatter},
    sync::{atomic::AtomicUsize, Arc},
};

use arrow::{array::*, datatypes::*};
use datafusion::{
    common::{Result, ScalarValue},
    physical_expr::PhysicalExpr,
};
use datafusion_ext_commons::downcast_any;
use paste::paste;

use crate::agg::{
    acc::{
        AccumInitialValue, AccumStateRow, AccumStateValAddr, AggDynBinary, AggDynScalar, AggDynStr,
        AggDynValue,
    },
    default_final_batch_merge_with_addr, default_final_merge_with_addr, Agg, WithAggBufAddrs,
    WithMemTracking,
};

pub struct AggFirst {
    child: Arc<dyn PhysicalExpr>,
    data_type: DataType,
    accums_initial: Vec<AccumInitialValue>,
    accum_state_val_addr_value: AccumStateValAddr,
    accum_state_val_addr_valid: AccumStateValAddr,
    partial_updater: fn(&Self, &mut AccumStateRow, &ArrayRef, usize),
    partial_buf_merger: fn(&Self, &mut AccumStateRow, &mut AccumStateRow),
    mem_used_tracker: AtomicUsize,
}

impl WithAggBufAddrs for AggFirst {
    fn set_accum_state_val_addrs(&mut self, accum_state_val_addrs: &[AccumStateValAddr]) {
        self.accum_state_val_addr_value = accum_state_val_addrs[0];
        self.accum_state_val_addr_valid = accum_state_val_addrs[1];
    }
}

impl WithMemTracking for AggFirst {
    fn mem_used_tracker(&self) -> &AtomicUsize {
        &self.mem_used_tracker
    }
}

impl AggFirst {
    pub fn try_new(child: Arc<dyn PhysicalExpr>, data_type: DataType) -> Result<Self> {
        let accums_initial = vec![
            AccumInitialValue::Scalar(ScalarValue::try_from(&data_type)?),
            AccumInitialValue::Scalar(ScalarValue::Null), // touched
        ];
        let partial_updater = get_partial_updater(&data_type)?;
        let partial_buf_merger = get_partial_buf_merger(&data_type)?;
        Ok(Self {
            child,
            data_type,
            accums_initial,
            accum_state_val_addr_value: AccumStateValAddr::default(),
            accum_state_val_addr_valid: AccumStateValAddr::default(),
            partial_updater,
            partial_buf_merger,
            mem_used_tracker: AtomicUsize::new(0),
        })
    }

    fn is_touched(&self, acc: &AccumStateRow) -> bool {
        acc.is_fixed_valid(self.accum_state_val_addr_valid)
    }

    fn set_touched(&self, acc: &mut AccumStateRow) {
        acc.set_fixed_valid(self.accum_state_val_addr_valid, true)
    }
}

impl Debug for AggFirst {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "First({:?})", self.child)
    }
}

impl Agg for AggFirst {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn exprs(&self) -> Vec<Arc<dyn PhysicalExpr>> {
        vec![self.child.clone()]
    }

    fn with_new_exprs(&self, exprs: Vec<Arc<dyn PhysicalExpr>>) -> Result<Arc<dyn Agg>> {
        Ok(Arc::new(Self::try_new(
            exprs[0].clone(),
            self.data_type.clone(),
        )?))
    }

    fn data_type(&self) -> &DataType {
        &self.data_type
    }

    fn nullable(&self) -> bool {
        true
    }

    fn accums_initial(&self) -> &[AccumInitialValue] {
        &self.accums_initial
    }

    fn partial_update(
        &self,
        acc: &mut AccumStateRow,
        values: &[ArrayRef],
        row_idx: usize,
    ) -> Result<()> {
        if !self.is_touched(acc) {
            let partial_updater = self.partial_updater;
            partial_updater(self, acc, &values[0], row_idx);
        }
        Ok(())
    }

    fn partial_update_all(&self, acc: &mut AccumStateRow, values: &[ArrayRef]) -> Result<()> {
        if !self.is_touched(acc) {
            let value = &values[0];
            if !value.is_empty() {
                let partial_updater = self.partial_updater;
                partial_updater(self, acc, value, 0);
            }
        }
        Ok(())
    }

    fn partial_merge(&self, acc1: &mut AccumStateRow, acc2: &mut AccumStateRow) -> Result<()> {
        let partial_buf_merger = self.partial_buf_merger;
        partial_buf_merger(self, acc1, acc2);
        Ok(())
    }

    fn final_merge(&self, acc: &mut AccumStateRow) -> Result<ScalarValue> {
        default_final_merge_with_addr(self, acc, self.accum_state_val_addr_value)
    }

    fn final_batch_merge(&self, accs: &mut [AccumStateRow]) -> Result<ArrayRef> {
        default_final_batch_merge_with_addr(self, accs, self.accum_state_val_addr_value)
    }
}

fn get_partial_updater(
    dt: &DataType,
) -> Result<fn(&AggFirst, &mut AccumStateRow, &ArrayRef, usize)> {
    // assert!(!is_touched(acc, addrs))

    macro_rules! fn_fixed {
        ($ty:ident) => {{
            Ok(|this, acc, v, i| {
                type TArray = paste! {[<$ty Array>]};
                if v.is_valid(i) {
                    let value = v.as_any().downcast_ref::<TArray>().unwrap();
                    acc.set_fixed_value(this.accum_state_val_addr_value, value.value(i));
                    acc.set_fixed_valid(this.accum_state_val_addr_value, true);
                }
                this.set_touched(acc);
            })
        }};
    }
    match dt {
        DataType::Null => Ok(|_, _, _, _| ()),
        DataType::Boolean => fn_fixed!(Boolean),
        DataType::Float32 => fn_fixed!(Float32),
        DataType::Float64 => fn_fixed!(Float64),
        DataType::Int8 => fn_fixed!(Int8),
        DataType::Int16 => fn_fixed!(Int16),
        DataType::Int32 => fn_fixed!(Int32),
        DataType::Int64 => fn_fixed!(Int64),
        DataType::UInt8 => fn_fixed!(UInt8),
        DataType::UInt16 => fn_fixed!(UInt16),
        DataType::UInt32 => fn_fixed!(UInt32),
        DataType::UInt64 => fn_fixed!(UInt64),
        DataType::Date32 => fn_fixed!(Date32),
        DataType::Date64 => fn_fixed!(Date64),
        DataType::Timestamp(TimeUnit::Second, _) => fn_fixed!(TimestampSecond),
        DataType::Timestamp(TimeUnit::Millisecond, _) => fn_fixed!(TimestampMillisecond),
        DataType::Timestamp(TimeUnit::Microsecond, _) => fn_fixed!(TimestampMicrosecond),
        DataType::Timestamp(TimeUnit::Nanosecond, _) => fn_fixed!(TimestampNanosecond),
        DataType::Decimal128(..) => fn_fixed!(Decimal128),
        DataType::Utf8 => Ok(
            |this: &AggFirst, acc: &mut AccumStateRow, v: &ArrayRef, i: usize| {
                if v.is_valid(i) {
                    let value = downcast_any!(v, StringArray).unwrap();
                    let v = value.value(i);
                    let new = AggDynStr::from_str(v);
                    this.add_mem_used(new.mem_size());
                    *acc.dyn_value_mut(this.accum_state_val_addr_value) = Some(Box::new(new));
                }
                this.set_touched(acc);
            },
        ),
        DataType::Binary => Ok(
            |this: &AggFirst, acc: &mut AccumStateRow, v: &ArrayRef, i: usize| {
                if v.is_valid(i) {
                    let value = downcast_any!(v, BinaryArray).unwrap();
                    let v = value.value(i);
                    let new = AggDynBinary::from_slice(v);
                    this.add_mem_used(new.mem_size());
                    *acc.dyn_value_mut(this.accum_state_val_addr_value) = Some(Box::new(new));
                }
                this.set_touched(acc);
            },
        ),
        _other => Ok(
            |this: &AggFirst, acc: &mut AccumStateRow, v: &ArrayRef, i: usize| {
                if v.is_valid(i) {
                    let v = ScalarValue::try_from_array(v, i)
                        .expect("First::partial_update error creating ScalarValue");
                    let new = AggDynScalar::new(v);
                    this.add_mem_used(new.mem_size());
                    *acc.dyn_value_mut(this.accum_state_val_addr_value) = Some(Box::new(new));
                }
                this.set_touched(acc);
            },
        ),
    }
}

fn get_partial_buf_merger(
    dt: &DataType,
) -> Result<fn(&AggFirst, &mut AccumStateRow, &mut AccumStateRow)> {
    // assert!(!is_touched(acc, addrs))

    macro_rules! fn_fixed {
        ($ty:ident) => {{
            Ok(|this, acc1, acc2| {
                type TType = paste! {[<$ty Type>]};
                type TNative = <TType as ArrowPrimitiveType>::Native;
                if this.is_touched(acc2) {
                    if acc2.is_fixed_valid(this.accum_state_val_addr_value) {
                        let value2 = acc2.fixed_value::<TNative>(this.accum_state_val_addr_value);
                        acc1.set_fixed_value(this.accum_state_val_addr_value, value2);
                        acc1.set_fixed_valid(this.accum_state_val_addr_value, true);
                    }
                    this.set_touched(acc1);
                }
            })
        }};
    }
    match dt {
        DataType::Null => Ok(|_, _, _| ()),
        DataType::Boolean => Ok(|this, acc1, acc2| {
            if this.is_touched(acc2) {
                if acc2.is_fixed_valid(this.accum_state_val_addr_value) {
                    acc1.set_fixed_value(
                        this.accum_state_val_addr_value,
                        acc2.fixed_value::<bool>(this.accum_state_val_addr_value),
                    );
                    acc1.set_fixed_valid(this.accum_state_val_addr_value, true);
                }
                this.set_touched(acc1);
            }
        }),
        DataType::Float32 => fn_fixed!(Float32),
        DataType::Float64 => fn_fixed!(Float64),
        DataType::Int8 => fn_fixed!(Int8),
        DataType::Int16 => fn_fixed!(Int16),
        DataType::Int32 => fn_fixed!(Int32),
        DataType::Int64 => fn_fixed!(Int64),
        DataType::UInt8 => fn_fixed!(UInt8),
        DataType::UInt16 => fn_fixed!(UInt16),
        DataType::UInt32 => fn_fixed!(UInt32),
        DataType::UInt64 => fn_fixed!(UInt64),
        DataType::Date32 => fn_fixed!(Date32),
        DataType::Date64 => fn_fixed!(Date64),
        DataType::Timestamp(TimeUnit::Second, _) => fn_fixed!(TimestampSecond),
        DataType::Timestamp(TimeUnit::Millisecond, _) => fn_fixed!(TimestampMillisecond),
        DataType::Timestamp(TimeUnit::Microsecond, _) => fn_fixed!(TimestampMicrosecond),
        DataType::Timestamp(TimeUnit::Nanosecond, _) => fn_fixed!(TimestampNanosecond),
        DataType::Decimal128(..) => fn_fixed!(Decimal128),
        DataType::Utf8 | DataType::Binary | _ => Ok(|this, acc1, acc2| {
            if this.is_touched(acc2) && !this.is_touched(acc1) {
                let w = acc1.dyn_value_mut(this.accum_state_val_addr_value);
                let v = acc2.dyn_value_mut(this.accum_state_val_addr_value);
                *w = std::mem::take(v);
            } else {
                if this.is_touched(acc2) {
                    if let Some(v) = acc2.dyn_value_mut(this.accum_state_val_addr_value) {
                        this.sub_mem_used(v.mem_size()); // v will be dropped
                    }
                }
            }
        }),
    }
}
