// Copyright (c) 2021 Quark Container Authors / 2018 The gVisor Authors.
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

use core::sync::atomic::{Ordering, AtomicU32};
use alloc::string::String;

use super::super::super::kernel_def::*;
use super::task::*;
use super::SHARESPACE;
use super::super::task_mgr::*;
use super::Kernel::HostSpace;
use super::TSC;
use super::super::linux_def::*;
use super::super::vcpu_mgr::*;
use super::threadmgr::task_sched::*;
use super::KERNEL_STACK_ALLOCATOR;
use super::quring::uring_mgr::*;
use super::Shutdown;
use super::ASYNC_PROCESS;

static ACTIVE_TASK: AtomicU32 = AtomicU32::new(0);

pub fn IncrActiveTask() -> u32 {
    return ACTIVE_TASK.fetch_add(1, Ordering::SeqCst);
}

pub fn DecrActiveTask() -> u32 {
    return ACTIVE_TASK.fetch_sub(1, Ordering::SeqCst);
}

pub fn AddNewCpu() {
    let mainTaskId = TaskStore::CreateFromThread();
    CPULocal::SetWaitTask(mainTaskId.Addr());
    CPULocal::SetCurrentTask(mainTaskId.Addr());
}

pub fn CreateTask(runFnAddr: u64, para: *const u8, kernel: bool) {
    let taskId = { TaskStore::CreateTask(runFnAddr, para, kernel) };
    SHARESPACE.scheduler.NewTask(taskId);

}

extern "C" {
    pub fn context_swap(_fromCxt: u64, _toCtx: u64, _one: u64, _zero: u64);
    pub fn context_swap_to(_fromCxt: u64, _toCtx: u64, _one: u64, _zero: u64);
}

fn switch_to(to: TaskId) {
    to.GetTask().AccountTaskLeave(SchedState::Blocked);

    CPULocal::SetCurrentTask(to.Addr());
    let toCtx = to.GetTask();

    toCtx.mm.VcpuEnter();

    if !SHARESPACE.config.read().KernelPagetable {
        toCtx.SwitchPageTable();
    }
    toCtx.SetFS();
    unsafe {
        context_swap_to(0, toCtx.GetContext(), 1, 0);
    }
}

pub const IO_WAIT_CYCLES : i64 = 20_000_000; // 1ms
pub const WAIT_CYCLES : i64 = 1_000_000; // 1ms

pub fn IOWait() {
    let mut start = TSC.Rdtsc();

    while !Shutdown() {
        if PollAsyncMsg() > 10 {
            start = TSC.Rdtsc();
        }

        let currentTime = TSC.Rdtsc();
        if currentTime - start >= IO_WAIT_CYCLES || Shutdown() {
            // after change the state, check again in case new message coming
            if PollAsyncMsg() > 10 && !Shutdown() {
                start = TSC.Rdtsc();
                continue;
            }

            //debug!("IOWait sleep");
            HostSpace::IOWait();
            //debug!("IOWait wakeup");
            start = TSC.Rdtsc();
        }
    }

    loop {
        HostSpace::IOWait();
    }
}

pub fn WaitFn() {
    let mut task = TaskId::default();
    loop {
        let next = if task.data == 0 {
            SHARESPACE.scheduler.GetNext()
        } else {
            let tmp = task;
            task = TaskId::default();
            Some(tmp)
        };

        match next {
            None => {
                SHARESPACE.scheduler.IncreaseHaltVcpuCnt();

                // if there is memory needs free and freed, continue free them
                // while super::ALLOCATOR.Free() {}

                if SHARESPACE.scheduler.GlobalReadyTaskCnt() == 0 {
                    //debug!("vcpu sleep");
                    let addr = HostSpace::VcpuWait();
                    //debug!("vcpu wakeup {:x}", addr);
                    assert!(addr >= 0);
                    task = TaskId::New(addr as u64);
                } else {
                    //error!("Waitfd None {}", SHARESPACE.scheduler.Print());
                }

                SHARESPACE.scheduler.DecreaseHaltVcpuCnt();
            }

            Some(newTask) => {
                let current = TaskId::New(CPULocal::CurrentTask());
                CPULocal::Myself().SwitchToRunning();
                switch(current, newTask);

                let pendingFreeStack = CPULocal::PendingFreeStack();
                if pendingFreeStack != 0 {
                    //(*PAGE_ALLOCATOR).Free(pendingFreeStack, DEFAULT_STACK_PAGES).unwrap();
                    KERNEL_STACK_ALLOCATOR.Free(pendingFreeStack).unwrap();
                    CPULocal::SetPendingFreeStack(0);
                }

                if Shutdown() {
                    //error!("shutdown: {}", super::AllocatorPrint(10));
                    super::Kernel::HostSpace::ExitVM(super::EXIT_CODE.load(QOrdering::SEQ_CST));
                }

                // todo: free heap cache
                //while super::ALLOCATOR.Free() {}
            }
        }
    }
}

#[inline]
pub fn PollAsyncMsg() -> usize {
    if Shutdown() {
        return 0;
    }

    let ret = QUringTrigger();
    if Shutdown() {
        return 0;
    }

    ASYNC_PROCESS.Process();

    //error!("PollAsyncMsg 4 count {}", ret);
    return ret;
}

#[inline]
pub fn ProcessOne() -> bool {
    return QUringProcessOne()
}

pub fn Wait() {
    CPULocal::Myself().ToSearch(&SHARESPACE);
    let start = TSC.Rdtsc();

    loop {
        let next = { SHARESPACE.scheduler.GetNext() };

        if let Some(newTask) = next {
            let current = TaskId::New(CPULocal::CurrentTask());
            //let vcpuId = newTask.GetTask().queueId;
            //assert!(CPULocal::CpuId()==vcpuId, "cpu {}, target cpu {}", CPULocal::CpuId(), vcpuId);

            CPULocal::Myself().SwitchToRunning();
            if current.data != newTask.data {
                switch(current, newTask);
            }

            break;
        }

        //super::ALLOCATOR.Free();

        let currentTime = TSC.Rdtsc();
        if currentTime - start >= WAIT_CYCLES {
            let current = TaskId::New(CPULocal::CurrentTask());
            let waitTask = TaskId::New(CPULocal::WaitTask());
            switch(current, waitTask);
            break;
        } else {
            if PollAsyncMsg() == 0 {
                unsafe { llvm_asm!("pause" :::: "volatile"); }
            }
        }
    }
}

pub fn SwitchToNewTask() -> ! {
    CPULocal::Myself().ToSearch(&SHARESPACE);

    let current = Task::TaskId();
    let waitTask = TaskId::New(CPULocal::WaitTask());
    switch(current, waitTask);
    panic!("SwitchToNewTask end impossible");
}

impl Scheduler {
    // steal scheduling
    pub fn GetNext(&self) -> Option<TaskId> {
        if self.GlobalReadyTaskCnt() == 0 {
            return None;
        }

        let vcpuId = CPULocal::CpuId() as usize;
        let vcpuCount = self.vcpuCnt;

        match self.GetNextForCpu(vcpuId, 0) {
            None => (),
            Some(t) => {
                return Some(t)
            }
        }

        /*match self.GetNextForCpu(vcpuId, vcpuId) {
            None => (),
            Some(t) => {
                return Some(t)
            }
        }*/

        for i in vcpuId ..vcpuId + vcpuCount {
            match self.GetNextForCpu(vcpuId, i % vcpuCount) {
                None => (),
                Some(t) => {
                    return Some(t)
                }
            }
        }

        return None;
    }

    pub fn Count(&self) -> u64 {
        let mut total = 0;
        let vcpuCount = self.vcpuCnt;
        for i in 0..vcpuCount {
            total += self.queue[i].Len();
        }

        return total;
    }

    pub fn Print(&self) -> String {
        let mut str = alloc::string::String::new();
        let vcpuCount = self.vcpuCnt;
        for i in 0..vcpuCount {
            if self.queue[i].Len() > 0 {
                str += &format!("{}:{}", i, self.queue[i].ToString());
            }
        }

        return str;
    }

    #[inline]
    pub fn GetNextForCpu(&self, currentCpuId: usize, vcpuId: usize) -> Option<TaskId> {
        // only stealing task from running VCPU
        if vcpuId != 0 && currentCpuId != vcpuId && CPULocal::GetCPUState(vcpuId) != VcpuState::Running {
            return None;
        }

        let count = self.queue[vcpuId].lock().len();
        for _ in 0..count {
            let task = {
                let mut queue = self.queue[vcpuId].lock();
                let task = queue.pop_front();
                if task.is_none() {
                    return None;
                }

                let _cnt = self.DecReadyTaskCount();
                task
            };

            let taskId = task.unwrap();

            assert!(vcpuId==taskId.GetTask().QueueId(),
            "vcpuId is {:x}, taskId.GetTask().QueueId() is {:x}, task {:x?}/{:x?}", vcpuId, taskId.GetTask().QueueId(), taskId, taskId.GetTask().guard);
            if taskId.GetTask().context.Ready() != 0 || taskId.data == Task::Current().taskId {
                //the task is in the queue, but the context has not been setup
                if currentCpuId != vcpuId { //stealing
                    //error!("cpu currentCpuId {} stealing task {:x?} from cpu {}", currentCpuId, taskId, vcpuId);

                    taskId.GetTask().SetQueueId(currentCpuId);
                } else {
                    if count > 1 { // current CPU has more task, try to wake other vcpu to handle
                        self.WakeOne();
                    }
                }

                //error!("GetNextForCpu task is {:x?}", taskId);
                return task
            }

            self.ScheduleQ(taskId, vcpuId as u64);
        }

        return None;
    }

    pub fn Schedule(&self, taskId: TaskId) {
        let vcpuId = taskId.GetTask().QueueId();
        //assert!(CPULocal::CpuId()==vcpuId, "cpu {}, target cpu {}", CPULocal::CpuId(), vcpuId);
        self.KScheduleQ(taskId, vcpuId);
    }

    pub fn KScheduleQ(&self, task: TaskId, vcpuId: usize) {
        //debug!("KScheduleQ task {:x?}, vcpuId {}", task, vcpuId);
        self.ScheduleQ(task, vcpuId as u64);
    }

    pub fn NewTask(&self, taskId: TaskId) -> usize {
        self.ScheduleQ(taskId, 0);
        return 0;
    }
}

pub fn Yield() {
    SHARESPACE.scheduler.Schedule(Task::TaskId());
    Wait();
}

pub fn NewTask(taskId: TaskId) {
    SHARESPACE.scheduler.NewTask(taskId);
}

pub fn ScheduleQ(taskId: TaskId) {
    SHARESPACE.scheduler.KScheduleQ(taskId, taskId.Queue() as usize);
}
