//! Reading a Linux host's accelerators: the command, and the parsers for
//! what `nvidia-smi` and `npu-smi` print.

use std::collections::HashMap;
use std::time::Duration;

use super::model::{Device, GpuProcess, Health, Reading, Vendor};
use crate::i18n::t;

/// The most seconds each tool gets before it is stopped. A GPU fallen off
/// the bus can leave `nvidia-smi` waiting on the driver; three runs of it
/// still end inside an exec's ten seconds.
const TIMEOUT: u32 = 3;

/// What `nvidia-smi` is asked of each GPU, in the order the CSV has it.
const NVIDIA_FIELDS: &str = "index,uuid,name,pci.bus_id,driver_version,pstate,\
                             utilization.gpu,memory.used,memory.total,temperature.gpu,\
                             power.draw,power.limit,fan.speed,\
                             ecc.errors.uncorrected.volatile.total";
const NVIDIA_COLUMNS: usize = 14;

/// What GPU/NPU runs on the host for a reading: `nvidia-smi` for the GPUs
/// and the processes computing on them, `npu-smi info` for Ascend NPUs,
/// each part under an `@@` line with how it exited after it, and once for
/// every process they named, `ps` for its owner and command line: the tools
/// give a program's path at best, and someone looking after a shared host
/// wants to know whose job it is.
///
/// First, how long the host has been up, which `read` takes from
/// `/proc/uptime` without starting anything. A tool missing is no part at
/// all; neither there, the output has nothing between that and `@@end`. `/usr/local/sbin` and `/sbin` go on the end of the `PATH`, where
/// Ascend's driver puts `npu-smi` and a command's shell does not look. Each
/// tool runs under `timeout` where the host has it.
///
/// One line, in single quotes for `sh -c`, so that whatever the login shell
/// is (bash, zsh, fish, csh) it hands the script to `sh` untouched: the
/// script has no single quote, and no `!` for csh to expand.
pub fn command() -> String {
    format!(
        "sh -c 'export LC_ALL=C; PATH=\"$PATH:/usr/local/sbin:/usr/sbin:/sbin\"; \
         if test -r /proc/uptime; then read -r up rest < /proc/uptime; echo @@uptime; echo \"$up\"; fi; \
         t=; if command -v timeout >/dev/null 2>&1; then t=\"timeout -s KILL {TIMEOUT}\"; fi; \
         pids=; \
         if command -v nvidia-smi >/dev/null 2>&1; then \
         out=$($t nvidia-smi --query-gpu={NVIDIA_FIELDS} --format=csv,noheader,nounits 2>&1); s=$?; \
         echo @@nvidia; printf \"%s\\n\" \"$out\"; echo \"@@status $s\"; \
         if test $s = 0; then \
         out=$($t nvidia-smi --query-compute-apps=gpu_uuid,pid,used_memory,process_name --format=csv,noheader,nounits 2>&1); s=$?; \
         echo @@nvidia-apps; printf \"%s\\n\" \"$out\"; echo \"@@status $s\"; \
         for p in $(printf \"%s\\n\" \"$out\" | cut -s -d, -f2); do \
         if test \"$p\" -gt 0 2>/dev/null; then pids=\"$pids,$p\"; fi; done; \
         fi; fi; \
         if command -v npu-smi >/dev/null 2>&1; then \
         out=$($t npu-smi info 2>&1); s=$?; \
         echo @@npu; printf \"%s\\n\" \"$out\"; echo \"@@status $s\"; \
         for p in $(printf \"%s\\n\" \"$out\" | cut -s -d\"|\" -f3); do \
         if test \"$p\" -gt 0 2>/dev/null; then pids=\"$pids,$p\"; fi; done; \
         fi; \
         if test -n \"$pids\"; then echo @@ps; ps -o pid=,user:32=,args= -p \"${{pids#,}}\" 2>/dev/null; fi; \
         echo @@end'"
    )
}

/// What the command's output said.
#[derive(Debug, PartialEq)]
pub enum Parsed {
    Reading(Box<Reading>),
    /// Neither `nvidia-smi` nor `npu-smi` is on the host.
    NoTools,
}

/// One tool's part of the output: its lines, and how it exited.
#[derive(Default)]
struct Part<'a> {
    lines: Vec<&'a str>,
    status: Option<i32>,
}

impl Part<'_> {
    fn failed(&self) -> bool {
        self.status.is_some_and(|status| status != 0)
    }

    /// `timeout` stopped it: 137 with `-s KILL`, 124 when it sent TERM.
    fn timed_out(&self) -> bool {
        matches!(self.status, Some(124 | 137))
    }
}

pub fn parse(output: &str) -> Parsed {
    let mut parts: HashMap<&str, Part> = HashMap::new();
    let mut current = None;
    for line in output.lines() {
        if let Some(status) = line.strip_prefix("@@status ") {
            if let Some(part) = current.and_then(|name| parts.get_mut(name)) {
                part.status = status.trim().parse().ok();
            }
        } else if let Some(name) = line.strip_prefix("@@") {
            let name = name.trim();
            parts.entry(name).or_default();
            current = Some(name);
        } else if let Some(part) = current.and_then(|name| parts.get_mut(name)) {
            part.lines.push(line);
        }
    }
    let (nvidia, npu) = (parts.get("nvidia"), parts.get("npu"));
    if nvidia.is_none() && npu.is_none() {
        return Parsed::NoTools;
    }

    let mut reading = Reading::default();
    let mut processes = Vec::new();
    if let Some(part) = nvidia {
        parse_nvidia(part, &mut reading);
        if let Some(apps) = parts.get("nvidia-apps") {
            parse_nvidia_apps(apps, &mut reading, &mut processes);
        }
    }
    if let Some(part) = npu {
        parse_npu(part, &mut reading, &mut processes);
    }
    reading.timed_out = parts.values().any(Part::timed_out);
    reading.uptime = parts
        .get("uptime")
        .and_then(|part| part.lines.first())
        .and_then(|line| line.trim().parse::<f64>().ok())
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.)
        .map(Duration::from_secs_f64);

    let owners: HashMap<u32, (&str, &str)> = parts
        .get("ps")
        .map(|part| {
            part.lines
                .iter()
                .filter_map(|line| parse_ps(line))
                .collect()
        })
        .unwrap_or_default();
    for process in &mut processes {
        if let Some((user, command)) = owners.get(&process.pid) {
            process.user = Some((*user).to_owned());
            process.command = Some((*command).to_owned());
        }
    }
    processes.sort_by(|a, b| {
        a.device
            .unwrap_or(usize::MAX)
            .cmp(&b.device.unwrap_or(usize::MAX))
            .then(b.memory.cmp(&a.memory))
            .then(a.pid.cmp(&b.pid))
    });
    reading.processes = processes;
    Parsed::Reading(Box::new(reading))
}

/// A number the tool printed, or `None` for its 「[N/A]」, 「[Not
/// Supported]」, 「ERR!」, 「NA」: never a zero it did not say.
fn number(text: &str) -> Option<f32> {
    text.trim()
        .parse::<f32>()
        .ok()
        .filter(|number| number.is_finite() && *number >= 0.)
}

fn mebibytes(text: &str) -> Option<u64> {
    number(text).map(|mib| (mib as f64 * 1024. * 1024.) as u64)
}

/// A word the tool printed, or `None` for its 「[N/A]」.
fn present(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty() && !text.starts_with('[') && text != "N/A").then(|| text.to_owned())
}

/// What a tool printed besides its answer, when it failed: why. When it
/// said nothing, how it exited.
fn errors(tool: &str, part: &Part, lines: Vec<&str>, reading: &mut Reading) {
    if !part.failed() {
        return;
    }
    let lines: Vec<String> = lines
        .into_iter()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    if part.timed_out() {
        reading
            .errors
            .push(t!("gpu.error.timed_out", tool = tool, seconds = TIMEOUT).into());
    } else if lines.is_empty() {
        reading.errors.push(
            t!(
                "gpu.error.status",
                tool = tool,
                status = part.status.unwrap_or_default()
            )
            .into(),
        );
    } else {
        reading.errors.extend(lines);
    }
}

/// `nvidia-smi --query-gpu`'s CSV: a line of `NVIDIA_COLUMNS` per GPU.
/// Anything else is the tool explaining itself (「Unable to determine the
/// device handle for GPU0000:1B:00.0: Unknown Error」).
fn parse_nvidia(part: &Part, reading: &mut Reading) {
    let mut other = Vec::new();
    for line in &part.lines {
        let fields: Vec<&str> = line.split(',').map(str::trim).collect();
        if fields.len() != NVIDIA_COLUMNS || fields[0].parse::<u32>().is_err() {
            other.push(*line);
            continue;
        }
        let uuid = present(fields[1]);
        let bus_id = present(fields[3]);
        let ecc = fields[13].parse::<u64>().ok();
        let label = format!("GPU {}", fields[0]);
        reading.devices.push(Device {
            vendor: Vendor::Nvidia,
            key: uuid
                .clone()
                .or_else(|| bus_id.clone())
                .unwrap_or(label.clone()),
            label,
            name: fields[2].to_owned(),
            bus_id,
            uuid,
            pstate: present(fields[5]),
            utilization: number(fields[6]),
            memory_used: mebibytes(fields[7]),
            memory_total: mebibytes(fields[8]),
            temperature: number(fields[9]),
            power: number(fields[10]),
            power_limit: number(fields[11]),
            fan: number(fields[12]),
            health: match ecc {
                Some(0) => Health::Ok,
                Some(errors) => Health::EccErrors(errors),
                None => Health::Unknown,
            },
        });
        if reading.drivers.is_empty()
            && let Some(driver) = present(fields[4])
        {
            reading.drivers.push(driver);
        }
    }
    errors("nvidia-smi", part, other, reading);
}

/// `nvidia-smi --query-compute-apps`: GPU UUID, PID, MiB, program.
fn parse_nvidia_apps(part: &Part, reading: &mut Reading, processes: &mut Vec<GpuProcess>) {
    let mut other = Vec::new();
    for line in &part.lines {
        let mut fields = line.splitn(4, ',').map(str::trim);
        let (Some(uuid), Some(Ok(pid)), Some(memory), Some(name)) = (
            fields.next(),
            fields.next().map(str::parse::<u32>),
            fields.next(),
            fields.next(),
        ) else {
            other.push(*line);
            continue;
        };
        processes.push(GpuProcess {
            device: reading
                .devices
                .iter()
                .position(|device| device.uuid.as_deref() == Some(uuid)),
            pid,
            name: name.to_owned(),
            user: None,
            command: None,
            memory: mebibytes(memory),
        });
    }
    errors("nvidia-smi", part, other, reading);
}

/// A card of `npu-smi info`'s table, as its first row says, with the chips
/// the rows under it list.
struct Card<'a> {
    npu: u32,
    name: String,
    health: &'a str,
    power: Option<f32>,
    temperature: Option<f32>,
    chips: Vec<Chip<'a>>,
}

struct Chip<'a> {
    id: u32,
    bus_id: &'a str,
    ai_core: Option<f32>,
    memory: Option<(u64, u64)>,
}

/// `npu-smi info`: a box-drawn table, two rows or more per card, then a
/// table of the processes on them.
///
/// ```text
/// | npu-smi 24.1.0.3                 Version: 24.1.0.3                    |
/// | NPU   Name         | Health   | Power(W)  Temp(C)  Hugepages-Usage(page) |
/// | Chip               | Bus-Id   | AICore(%) Memory-Usage(MB) HBM-Usage(MB) |
/// | 0     910B3        | OK       | 92.9      37       0    / 0             |
/// | 0                  | 0000:C1:00.0 | 0     0    / 0   3379 / 65536      |
/// | NPU     Chip       | Process id | Process name | Process memory(MB)    |
/// | 0       0          | 12345      | python3.8    | 17400                 |
/// ```
///
/// A row whose second cell is a bus id is a chip of the card above it; a
/// card holds one chip (910B) or more (310P, 910 A3), each its own device.
/// The memory is the HBM where a chip has some, else its DDR (310P).
fn parse_npu(part: &Part, reading: &mut Reading, processes: &mut Vec<GpuProcess>) {
    let mut cards: Vec<Card> = Vec::new();
    let mut found: Vec<(u32, u32, u32, &str, Option<u64>)> = Vec::new();
    let mut other = Vec::new();
    for line in &part.lines {
        let line = line.trim();
        if line.starts_with('+') || line.is_empty() {
            continue;
        }
        let Some(inner) = line
            .strip_prefix('|')
            .and_then(|line| line.strip_suffix('|'))
        else {
            other.push(line);
            continue;
        };
        if let Some((_, version)) = inner.split_once("Version:") {
            if let Some(version) = version.split_whitespace().next() {
                reading.drivers.push(version.to_owned());
            }
            continue;
        }
        let cells: Vec<&str> = inner.split('|').map(str::trim).collect();
        let mut first = cells[0].split_whitespace();
        let Some(Ok(id)) = first.next().map(str::parse::<u32>) else {
            // A heading, or 「No running processes found in NPU 0」.
            continue;
        };
        match cells.as_slice() {
            [_, bus_id, figures] if bus_id.contains(':') => {
                let Some(card) = cards.last_mut() else {
                    continue;
                };
                let figures: Vec<&str> = figures
                    .split(|c: char| c == '/' || c.is_whitespace())
                    .filter(|figure| !figure.is_empty())
                    .collect();
                let pair = |at: usize| -> Option<(u64, u64)> {
                    let (used, total) = (
                        mebibytes(figures.get(at)?)?,
                        mebibytes(figures.get(at + 1)?)?,
                    );
                    (total > 0).then_some((used, total))
                };
                card.chips.push(Chip {
                    id,
                    bus_id,
                    ai_core: figures.first().and_then(|figure| number(figure)),
                    memory: pair(3).or_else(|| pair(1)),
                });
            }
            [_, health, figures] => {
                let mut figures = figures.split_whitespace();
                cards.push(Card {
                    npu: id,
                    name: first.collect::<Vec<_>>().join(" "),
                    health,
                    power: figures.next().and_then(number),
                    temperature: figures.next().and_then(number),
                    chips: Vec::new(),
                });
            }
            [_, pid, name, memory] => {
                let (Some(Ok(chip)), Ok(pid)) =
                    (first.next().map(str::parse::<u32>), pid.parse::<u32>())
                else {
                    continue;
                };
                found.push((id, chip, pid, name, mebibytes(memory)));
            }
            _ => {}
        }
    }

    let start = reading.devices.len();
    let mut places = HashMap::new();
    for card in &cards {
        for chip in &card.chips {
            let label = if card.chips.len() > 1 {
                format!("NPU {}.{}", card.npu, chip.id)
            } else {
                format!("NPU {}", card.npu)
            };
            places.insert((card.npu, chip.id), reading.devices.len());
            reading.devices.push(Device {
                vendor: Vendor::Ascend,
                key: chip.bus_id.to_owned(),
                label,
                name: card.name.clone(),
                bus_id: Some(chip.bus_id.to_owned()),
                uuid: None,
                pstate: None,
                utilization: chip.ai_core,
                memory_used: chip.memory.map(|memory| memory.0),
                memory_total: chip.memory.map(|memory| memory.1),
                temperature: card.temperature,
                power: card.power,
                power_limit: None,
                fan: None,
                health: if card.health.eq_ignore_ascii_case("OK") {
                    Health::Ok
                } else {
                    Health::Reported(card.health.to_owned())
                },
            });
        }
    }
    for (npu, chip, pid, name, memory) in found {
        processes.push(GpuProcess {
            device: places.get(&(npu, chip)).copied(),
            pid,
            name: name.to_owned(),
            user: None,
            command: None,
            memory,
        });
    }
    if reading.devices.len() == start && !part.failed() && !other.is_empty() {
        // It ran, but what it printed is no table: say what it said.
        reading
            .errors
            .extend(other.iter().map(|line| (*line).to_owned()));
        return;
    }
    errors("npu-smi", part, other, reading);
}

/// 「  12345 alice    python train.py --epochs 3」.
fn parse_ps(line: &str) -> Option<(u32, (&str, &str))> {
    let line = line.trim_start();
    let (pid, rest) = line.split_once(char::is_whitespace)?;
    let (user, command) = rest.trim_start().split_once(char::is_whitespace)?;
    Some((pid.parse().ok()?, (user, command.trim())))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two of eight H100s, the second with its fan and ECC unsaid, and two
    /// ranks of a job on them.
    const NVIDIA: &str = "\
@@nvidia
0, GPU-2cce7924-7ee9-7245-7e6a-11c2a73bd842, NVIDIA H100 80GB HBM3, 00000000:18:00.0, 550.54.15, P0, 97, 62668, 81559, 71, 512.34, 700.00, [N/A], 0
1, GPU-5b2f0c1e-0000-1111-2222-333344445555, NVIDIA H100 80GB HBM3, 00000000:2A:00.0, 550.54.15, P8, 0, 1, 81559, 32, 69.10, 700.00, [N/A], [N/A]
@@status 0
@@nvidia-apps
GPU-2cce7924-7ee9-7245-7e6a-11c2a73bd842, 4242, 60000, /usr/bin/python3
GPU-2cce7924-7ee9-7245-7e6a-11c2a73bd842, 4300, 2000, /usr/bin/python3
GPU-5b2f0c1e-0000-1111-2222-333344445555, 4243, 1000, /usr/bin/python3
@@status 0
@@ps
   4242 alice    torchrun --nproc 8 train.py
   4243 alice    torchrun --nproc 8 train.py
@@end
";

    /// `npu-smi info` on an Atlas 800T A2: two of eight 910B3s, a process
    /// on the second. After the samples Huawei and others publish.
    const ASCEND: &str = "\
@@npu
+------------------------------------------------------------------------------------------------+
| npu-smi 24.1.0.3                 Version: 24.1.0.3                                             |
+---------------------------+---------------+----------------------------------------------------+
| NPU   Name                | Health        | Power(W)    Temp(C)           Hugepages-Usage(page)|
| Chip                      | Bus-Id        | AICore(%)   Memory-Usage(MB)  HBM-Usage(MB)        |
+===========================+===============+====================================================+
| 0     910B3               | OK            | 92.9        37                0    / 0             |
| 0                         | 0000:C1:00.0  | 0           0    / 0          3379 / 65536         |
+===========================+===============+====================================================+
| 1     910B3               | Warning       | 345.0       58                0    / 0             |
| 0                         | 0000:C2:00.0  | 87          0    / 0          62000/ 65536         |
+===========================+===============+====================================================+
+---------------------------+---------------+----------------------------------------------------+
| NPU     Chip              | Process id    | Process name             | Process memory(MB)      |
+===========================+===============+====================================================+
| No running processes found in NPU 0                                                            |
+===========================+===============+====================================================+
| 1       0                 | 31337         | python3.8                | 58600                   |
+===========================+===============+====================================================+
@@status 0
@@ps
  31337 HwHiAiUser python3.8 infer.py
@@end
";

    /// An Atlas 300I Duo: two 310P3 chips on one card, DDR and no HBM, no
    /// power reading.
    const DUO: &str = "\
@@npu
+--------------------------------------------------------------------------------------------------------+
| npu-smi 23.0.rc3                                 Version: 23.0.rc3                                     |
+-------------------------------+-----------------+------------------------------------------------------+
| NPU     Name                  | Health          | Power(W)     Temp(C)           Hugepages-Usage(page) |
| Chip    Device                | Bus-Id          | AICore(%)    Memory-Usage(MB)                        |
+===============================+=================+======================================================+
| 1       310P3                 | OK              | NA           53                0     / 0             |
| 0       0                     | 0000:3B:00.0    | 0            1610 / 21527                            |
| 1       1                     | 0000:3C:00.0    | 12           1543 / 21527                            |
+===============================+=================+======================================================+
+-------------------------------+-----------------+------------------------------------------------------+
| NPU     Chip                  | Process id      | Process name             | Process memory(MB)        |
+===============================+=================+======================================================+
| 1       1                     | 777             | mxVision                 | 300                       |
+===============================+=================+======================================================+
@@status 0
@@end
";

    fn reading(output: &str) -> Reading {
        match parse(output) {
            Parsed::Reading(reading) => *reading,
            Parsed::NoTools => panic!("no tools in {output}"),
        }
    }

    const MIB: u64 = 1 << 20;

    #[test]
    fn nvidia_smi_gives_each_gpu_and_what_computes_on_it() {
        let reading = reading(NVIDIA);
        assert_eq!(reading.drivers, ["550.54.15"]);
        assert!(reading.errors.is_empty());
        let [busy, idle] = reading.devices.as_slice() else {
            panic!("two GPUs: {:?}", reading.devices);
        };
        assert_eq!(busy.label, "GPU 0");
        assert_eq!(busy.key, "GPU-2cce7924-7ee9-7245-7e6a-11c2a73bd842");
        assert_eq!(busy.name, "NVIDIA H100 80GB HBM3");
        assert_eq!(busy.bus_id.as_deref(), Some("00000000:18:00.0"));
        assert_eq!(busy.pstate.as_deref(), Some("P0"));
        assert_eq!(busy.utilization, Some(97.));
        assert_eq!(busy.memory_used, Some(62_668 * MIB));
        assert_eq!(busy.memory_total, Some(81_559 * MIB));
        assert_eq!(busy.temperature, Some(71.));
        assert_eq!(busy.power, Some(512.34));
        assert_eq!(busy.power_limit, Some(700.));
        assert_eq!(busy.fan, None, "[N/A] is no reading, not zero");
        assert_eq!(busy.health, Health::Ok);
        assert_eq!(idle.health, Health::Unknown, "no ECC");

        // By device, the most memory first; `ps` saw two of them.
        let processes: Vec<(Option<usize>, u32)> = reading
            .processes
            .iter()
            .map(|process| (process.device, process.pid))
            .collect();
        assert_eq!(
            processes,
            [(Some(0), 4242), (Some(0), 4300), (Some(1), 4243)]
        );
        let owners: Vec<(Option<&str>, Option<&str>)> = reading
            .processes
            .iter()
            .map(|process| (process.user.as_deref(), process.command.as_deref()))
            .collect();
        let job = (Some("alice"), Some("torchrun --nproc 8 train.py"));
        assert_eq!(owners, [job, (None, None), job]);
        assert_eq!(reading.processes[1].title(), "/usr/bin/python3");
        assert_eq!(reading.processes[0].memory, Some(60_000 * MIB));
    }

    #[test]
    fn uncorrectable_ecc_errors_are_ill_health() {
        let output = NVIDIA.replace("[N/A], [N/A]\n", "[N/A], 3\n");
        assert_eq!(reading(&output).devices[1].health, Health::EccErrors(3));
    }

    #[test]
    fn what_nvidia_smi_says_instead_of_answering_shows_as_it_said_it() {
        let reading = reading(
            "@@nvidia\n\
             Unable to determine the device handle for GPU0000:1B:00.0: Unknown Error\n\
             @@status 15\n@@end\n",
        );
        assert!(reading.devices.is_empty());
        assert_eq!(
            reading.errors,
            ["Unable to determine the device handle for GPU0000:1B:00.0: Unknown Error"]
        );
        assert!(!reading.timed_out);

        let reading = self::reading(
            "@@nvidia\nNVIDIA-SMI has failed because it couldn't communicate with the NVIDIA driver.\n\
             @@status 9\n@@end\n",
        );
        assert_eq!(reading.errors.len(), 1);

        let silent = self::reading("@@nvidia\n\n@@status 6\n@@end\n");
        assert_eq!(
            silent.errors,
            [t!("gpu.error.status", tool = "nvidia-smi", status = 6).to_string()]
        );
    }

    #[test]
    fn a_tool_stopped_for_taking_too_long_says_so() {
        let reading = reading("@@nvidia\n\n@@status 137\n@@end\n");
        assert!(reading.timed_out);
        assert_eq!(
            reading.errors,
            [t!(
                "gpu.error.timed_out",
                tool = "nvidia-smi",
                seconds = TIMEOUT
            )
            .to_string()]
        );
    }

    #[test]
    fn npu_smi_gives_each_card_its_chip_and_the_processes() {
        let reading = reading(ASCEND);
        assert_eq!(reading.drivers, ["24.1.0.3"]);
        assert!(reading.errors.is_empty(), "{:?}", reading.errors);
        let [first, second] = reading.devices.as_slice() else {
            panic!("two NPUs: {:?}", reading.devices);
        };
        assert_eq!(first.label, "NPU 0");
        assert_eq!(first.vendor, Vendor::Ascend);
        assert_eq!(first.name, "910B3");
        assert_eq!(first.key, "0000:C1:00.0");
        assert_eq!(first.utilization, Some(0.));
        assert_eq!(first.memory_used, Some(3379 * MIB));
        assert_eq!(first.memory_total, Some(65_536 * MIB));
        assert_eq!(first.power, Some(92.9));
        assert_eq!(first.temperature, Some(37.));
        assert_eq!(first.health, Health::Ok);
        assert_eq!(second.utilization, Some(87.));
        assert_eq!(second.memory_used, Some(62_000 * MIB), "62000/ 65536");
        assert_eq!(second.health, Health::Reported("Warning".into()));

        let [process] = reading.processes.as_slice() else {
            panic!("one process: {:?}", reading.processes);
        };
        assert_eq!(process.device, Some(1));
        assert_eq!(process.pid, 31337);
        assert_eq!(process.memory, Some(58_600 * MIB));
        assert_eq!(process.user.as_deref(), Some("HwHiAiUser"));
        assert_eq!(process.title(), "python3.8 infer.py");
    }

    #[test]
    fn a_card_of_two_chips_is_two_devices() {
        let reading = reading(DUO);
        let labels: Vec<&str> = reading.devices.iter().map(|d| d.label.as_str()).collect();
        assert_eq!(labels, ["NPU 1.0", "NPU 1.1"]);
        let second = &reading.devices[1];
        assert_eq!(second.name, "310P3");
        assert_eq!(second.power, None, "NA");
        assert_eq!(second.temperature, Some(53.));
        assert_eq!(second.utilization, Some(12.));
        assert_eq!(second.memory_used, Some(1543 * MIB), "DDR, with no HBM");
        assert_eq!(second.memory_total, Some(21_527 * MIB));
        assert_eq!(reading.processes[0].device, Some(1));
        assert_eq!(reading.processes[0].title(), "mxVision", "no ps part");
    }

    #[test]
    fn what_npu_smi_says_instead_of_a_table_shows() {
        let denied = reading("@@npu\nPermission denied, please run as root.\n@@status 1\n@@end\n");
        assert_eq!(denied.errors, ["Permission denied, please run as root."]);
        let odd =
            reading("@@npu\ndcmi module initialize failed. ret is -8005\n@@status 0\n@@end\n");
        assert_eq!(odd.errors, ["dcmi module initialize failed. ret is -8005"]);
    }

    #[test]
    fn a_host_with_both_lists_both() {
        let both = reading(&format!(
            "{}{}",
            NVIDIA.replace("@@end\n", ""),
            ASCEND.replace("@@ps\n  31337 HwHiAiUser python3.8 infer.py\n", "")
        ));
        let labels: Vec<&str> = both.devices.iter().map(|d| d.label.as_str()).collect();
        assert_eq!(labels, ["GPU 0", "GPU 1", "NPU 0", "NPU 1"]);
        assert_eq!(both.drivers, ["550.54.15", "24.1.0.3"]);
        assert_eq!(both.processes.last().unwrap().device, Some(3));
    }

    #[test]
    fn neither_tool_is_nothing_to_read() {
        assert_eq!(parse("@@uptime\n120.5\n@@end\n"), Parsed::NoTools);
        assert_eq!(parse("@@end\n"), Parsed::NoTools);
        assert_eq!(parse(""), Parsed::NoTools);
    }

    #[test]
    fn the_host_says_how_long_it_has_been_up() {
        let up = reading(&format!("@@uptime\n5359162.33\n{NVIDIA}"));
        assert_eq!(up.uptime, Some(Duration::from_secs_f64(5359162.33)));
        assert_eq!(up.devices.len(), 2);
        assert_eq!(reading(NVIDIA).uptime, None);
    }

    #[test]
    fn the_command_is_one_line_sh_reads_whatever_the_login_shell() {
        let command = command();
        assert!(!command.contains('\n'));
        assert_eq!(command.matches('\'').count(), 2, "{command}");
        assert!(!command.contains('!'));
    }

    #[cfg(unix)]
    fn script(command: &str) -> &str {
        command
            .strip_prefix("sh -c '")
            .and_then(|rest| rest.strip_suffix('\''))
            .unwrap()
    }

    /// The real `cut` and `sh`: the tools the script uses that the test
    /// does not stand in for.
    #[cfg(unix)]
    fn real_tool(name: &str) -> std::path::PathBuf {
        std::env::var_os("PATH")
            .as_deref()
            .map(std::env::split_paths)
            .into_iter()
            .flatten()
            .map(|dir| dir.join(name))
            .find(|path| path.is_file())
            .unwrap_or_else(|| std::path::PathBuf::from("/bin").join(name))
    }

    #[cfg(unix)]
    #[test]
    fn the_command_is_valid_sh() {
        assert!(crate::testing::sh_accepts(script(&command())));
    }

    /// The script, run by this machine's `sh` with the tools stood in for,
    /// asks them what the parsers read, and `ps` for the processes they
    /// name: the PIDs from both tables, and nothing else from them.
    #[cfg(unix)]
    #[test]
    fn the_script_asks_the_tools_and_ps() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().expect("temp dir");
        let bin = root.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let tool = |name: &str, body: &str| {
            let path = bin.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        let nvidia_gpus = NVIDIA
            .split("@@nvidia\n")
            .nth(1)
            .and_then(|rest| rest.split("@@status").next())
            .unwrap();
        let nvidia_apps = NVIDIA
            .split("@@nvidia-apps\n")
            .nth(1)
            .and_then(|rest| rest.split("@@status").next())
            .unwrap();
        let npu_table = ASCEND
            .split("@@npu\n")
            .nth(1)
            .and_then(|rest| rest.split("@@status").next())
            .unwrap();
        let fields = format!("--query-gpu={NVIDIA_FIELDS}");
        tool(
            "nvidia-smi",
            &format!(
                "test \"$2\" = --format=csv,noheader,nounits || exit 2\n\
                 case \"$1\" in\n\
                 {fields}) printf '%s' '{nvidia_gpus}';;\n\
                 --query-compute-apps=gpu_uuid,pid,used_memory,process_name) printf '%s' '{nvidia_apps}';;\n\
                 *) exit 2;;\n\
                 esac\n"
            ),
        );
        tool(
            "npu-smi",
            &format!("test \"$1\" = info || exit 2\nprintf '%s' '{npu_table}'\n"),
        );
        // Says what it was asked, for the test to read.
        tool("ps", "echo \"   1 asked $*\"\n");
        tool(
            "timeout",
            "test \"$1 $2 $3\" = \"-s KILL 3\" || exit 2\nshift 3\nexec \"$@\"\n",
        );
        let uptime = root.path().join("uptime");
        std::fs::write(&uptime, "5359162.33 1.00\n").unwrap();
        // The PATH is the test's own: the machine's tools must not answer
        // for themselves, or a machine with an NVIDIA card of its own
        // would say the tool is there when the test means it is not. The
        // tools the test does not stand in for are `cut`, which the
        // script uses to read the processes' numbers out of the table,
        // and the `sh` that runs it.
        let tools = root.path().join("tools");
        std::fs::create_dir_all(&tools).unwrap();
        for name in ["cut", "sh"] {
            std::os::unix::fs::symlink(real_tool(name), tools.join(name)).unwrap();
        }
        let command = command()
            .replace("/proc/uptime", &uptime.display().to_string())
            .replace(
                "/usr/local/sbin:/usr/sbin:/sbin",
                &tools.display().to_string(),
            );
        let path = format!("{}:{}", bin.display(), tools.display());
        let output = crate::testing::sh(script(&command), Some(&path));
        let output = String::from_utf8(output.stdout).unwrap();

        let reading = reading(&output);
        assert_eq!(
            reading.uptime,
            Some(Duration::from_secs_f64(5359162.33)),
            "{output}"
        );
        assert_eq!(reading.devices.len(), 4, "{output}");
        assert!(reading.errors.is_empty(), "{output}");
        assert_eq!(reading.processes.len(), 4);
        assert!(
            output.contains("   1 asked -o pid=,user:32=,args= -p 4242,4300,4243,31337\n"),
            "{output}"
        );
        assert!(output.ends_with("@@end\n"));

        // Neither tool: the uptime, then the end.
        std::fs::remove_file(bin.join("nvidia-smi")).unwrap();
        std::fs::remove_file(bin.join("npu-smi")).unwrap();
        let output = crate::testing::sh(script(&command), Some(&path));
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "@@uptime\n5359162.33\n@@end\n"
        );
    }
}
