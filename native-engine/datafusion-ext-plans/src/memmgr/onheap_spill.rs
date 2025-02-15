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
    fs::File,
    io::{BufReader, BufWriter, Read, Seek, Write},
    sync::Arc,
    time::Duration,
};

use blaze_jni_bridge::{
    is_jni_bridge_inited, jni_call, jni_call_static, jni_new_direct_byte_buffer, jni_new_global_ref,
};
use datafusion::{common::Result, parquet::file::reader::Length, physical_plan::metrics::Time};
use jni::{objects::GlobalRef, sys::jlong};

use crate::memmgr::metrics::SpillMetrics;

pub trait Spill: Send + Sync {
    fn complete(&self) -> Result<()>;
    fn get_buf_reader(&self) -> BufReader<Box<dyn Read + Send>>;
    fn get_buf_writer(&self) -> BufWriter<Box<dyn Write + Send>>;
}

pub fn try_new_spill(spill_metrics: &SpillMetrics) -> Result<Box<dyn Spill>> {
    if !is_jni_bridge_inited() || jni_call_static!(JniBridge.isDriverSide() -> bool)? {
        Ok(Box::new(FileSpill::try_new(spill_metrics)?))
    } else {
        Ok(Box::new(OnHeapSpill::try_new(spill_metrics)?))
    }
}

/// A spill structure which write data to temporary files
/// used in driver side
struct FileSpill(File, SpillMetrics);
impl FileSpill {
    fn try_new(spill_metrics: &SpillMetrics) -> Result<Self> {
        let file = tempfile::tempfile()?;
        Ok(Self(file, spill_metrics.clone()))
    }
}

impl Spill for FileSpill {
    fn complete(&self) -> Result<()> {
        let mut file_cloned = self.0.try_clone().expect("File.try_clone() returns error");
        file_cloned.sync_data()?;
        file_cloned.rewind()?;
        Ok(())
    }

    fn get_buf_reader(&self) -> BufReader<Box<dyn Read + Send>> {
        let file_cloned = self.0.try_clone().expect("File.try_clone() returns error");
        BufReader::with_capacity(
            65536,
            Box::new(IoTimeReadWrapper(
                file_cloned,
                self.1.mem_spill_iotime.clone(),
            )),
        )
    }

    fn get_buf_writer(&self) -> BufWriter<Box<dyn Write + Send>> {
        let file_cloned = self.0.try_clone().expect("File.try_clone() returns error");
        BufWriter::with_capacity(
            65536,
            Box::new(IoTimeWriteWrapper(
                file_cloned,
                self.1.mem_spill_iotime.clone(),
            )),
        )
    }
}

impl Drop for FileSpill {
    fn drop(&mut self) {
        // values of mem spill size/iotime are the same with disk spill
        self.1.mem_spill_size.add(self.0.len() as usize);
        self.1.disk_spill_size.add(self.0.len() as usize);
        self.1
            .mem_spill_iotime
            .add_duration(Duration::from_nanos(self.1.mem_spill_iotime.value() as u64))
    }
}

/// A spill structure which cooperates with BlazeOnHeapSpillManager
/// used in executor side
struct OnHeapSpill(Arc<RawOnHeapSpill>, SpillMetrics);
impl OnHeapSpill {
    fn try_new(spill_metrics: &SpillMetrics) -> Result<Self> {
        let hsm = jni_call_static!(JniBridge.getTaskOnHeapSpillManager() -> JObject)?;
        let spill_id = jni_call!(BlazeOnHeapSpillManager(hsm.as_obj()).newSpill() -> i32)?;

        Ok(Self(
            Arc::new(RawOnHeapSpill {
                hsm: jni_new_global_ref!(hsm.as_obj())?,
                spill_id,
            }),
            spill_metrics.clone(),
        ))
    }

    fn get_disk_usage(&self) -> Result<u64> {
        let usage = jni_call!(BlazeOnHeapSpillManager(self.0.hsm.as_obj())
            .getSpillDiskUsage(self.0.spill_id) -> jlong)? as u64;
        Ok(usage)
    }

    fn get_disk_iotime(&self) -> Result<u64> {
        let iotime = jni_call!(BlazeOnHeapSpillManager(self.0.hsm.as_obj())
            .getSpillDiskIOTime(self.0.spill_id) -> jlong)? as u64;
        Ok(iotime)
    }
}

impl Spill for OnHeapSpill {
    fn complete(&self) -> Result<()> {
        jni_call!(BlazeOnHeapSpillManager(self.0.hsm.as_obj())
            .completeSpill(self.0.spill_id) -> ())?;
        Ok(())
    }

    fn get_buf_reader(&self) -> BufReader<Box<dyn Read + Send>> {
        let cloned = Self(self.0.clone(), self.1.clone());
        BufReader::with_capacity(65536, Box::new(cloned))
    }

    fn get_buf_writer(&self) -> BufWriter<Box<dyn Write + Send>> {
        let cloned = Self(self.0.clone(), self.1.clone());
        BufWriter::with_capacity(65536, Box::new(cloned))
    }
}

impl Write for OnHeapSpill {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _timer = self.1.mem_spill_iotime.timer();
        let write_len = buf.len();
        let buf = jni_new_direct_byte_buffer!(buf)?;

        jni_call!(BlazeOnHeapSpillManager(
            self.0.hsm.as_obj()).writeSpill(self.0.spill_id, buf.as_obj()) -> ()
        )?;
        self.1.mem_spill_size.add(write_len);
        Ok(write_len)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Read for OnHeapSpill {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let _timer = self.1.mem_spill_iotime.timer();
        let buf = jni_new_direct_byte_buffer!(buf)?;
        let read_len = jni_call!(BlazeOnHeapSpillManager(
            self.0.hsm.as_obj()).readSpill(self.0.spill_id, buf.as_obj()) -> i32
        )?;
        Ok(read_len as usize)
    }
}

impl Drop for OnHeapSpill {
    fn drop(&mut self) {
        self.1.mem_spill_count.add(1);
        self.1
            .disk_spill_size
            .add(self.get_disk_usage().unwrap_or(0) as usize);
        self.1
            .disk_spill_iotime
            .add_duration(Duration::from_nanos(self.get_disk_iotime().unwrap_or(0)));
    }
}

struct RawOnHeapSpill {
    hsm: GlobalRef,
    spill_id: i32,
}

impl Drop for RawOnHeapSpill {
    fn drop(&mut self) {
        let _ = jni_call!(BlazeOnHeapSpillManager(self.hsm.as_obj())
            .releaseSpill(self.spill_id) -> ());
    }
}

struct IoTimeReadWrapper<R: Read>(R, Time);
struct IoTimeWriteWrapper<W: Write>(W, Time);

impl<R: Read> Read for IoTimeReadWrapper<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let _timer = self.1.timer();
        self.0.read(buf)
    }
}

impl<W: Write> Write for IoTimeWriteWrapper<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _timer = self.1.timer();
        self.0.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let _timer = self.1.timer();
        self.0.flush()
    }
}
