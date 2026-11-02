use super::rewrite::Request;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pool {
    pub cpus: f64,
    pub memory_bytes: u64,
    pub storage_bytes: Option<u64>,
}

/// What a running container counts against the pool: its recent peak use, or its limits before it has samples.
#[derive(Debug, Clone, PartialEq)]
pub struct Running {
    pub deployment: String,
    pub cpus: f64,
    pub memory_bytes: u64,
}

pub fn admit(
    pool: Pool,
    running: &[Running],
    storage_used: u64,
    deployment: &str,
    requests: &[Request],
) -> Result<(), String> {
    let others = running.iter().filter(|r| r.deployment != deployment);
    let used_cpus = others.clone().fold(0.0, |sum, r| sum + r.cpus);
    let used_memory: u64 = others.map(|r| r.memory_bytes).sum();
    let need_cpus = requests.iter().fold(0.0, |sum, r| sum + r.cpus * r.replicas as f64);
    let need_memory: u64 = requests.iter().map(|r| r.memory_bytes * r.replicas).sum();

    let mut problems = Vec::new();
    if used_memory + need_memory > pool.memory_bytes {
        problems.push(format!(
            "memory pool is {}, {} is in use and this deployment asks for up to {}",
            gib(pool.memory_bytes),
            gib(used_memory),
            gib(need_memory)
        ));
    }
    if used_cpus + need_cpus > pool.cpus + 1e-9 {
        problems.push(format!(
            "CPU pool is {:.2}, {:.2} is in use and this deployment asks for up to {:.2}",
            pool.cpus, used_cpus, need_cpus
        ));
    }
    if let Some(limit) = pool.storage_bytes
        && storage_used >= limit
    {
        problems.push(format!("storage pool is {}, {} is already used", gib(limit), gib(storage_used)));
    }
    if problems.is_empty() { Ok(()) } else { Err(problems.join(". ")) }
}

fn gib(bytes: u64) -> String {
    format!("{:.2} GiB", bytes as f64 / (1u64 << 30) as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    fn pool() -> Pool {
        Pool { cpus: 4.0, memory_bytes: 8 * GIB, storage_bytes: Some(100 * GIB) }
    }

    fn request(cpus: f64, memory_bytes: u64, replicas: u64) -> Request {
        Request { service: "web".into(), replicas, cpus, memory_bytes }
    }

    fn running(deployment: &str, cpus: f64, memory_bytes: u64) -> Running {
        Running { deployment: deployment.into(), cpus, memory_bytes }
    }

    #[test]
    fn fits_exactly() {
        let running = [running("a", 2.0, 6 * GIB)];
        assert_eq!(admit(pool(), &running, 0, "b", &[request(1.0, GIB, 2)]), Ok(()));
    }

    #[test]
    fn memory_overcommit_names_the_numbers() {
        let running = [running("a", 1.0, 6 * GIB)];
        let err = admit(pool(), &running, 0, "b", &[request(0.5, 3 * GIB, 1)]).unwrap_err();
        assert_eq!(err, "memory pool is 8.00 GiB, 6.00 GiB is in use and this deployment asks for up to 3.00 GiB");
    }

    #[test]
    fn replicas_multiply() {
        let err = admit(pool(), &[], 0, "b", &[request(1.5, GIB, 3)]).unwrap_err();
        assert_eq!(err, "CPU pool is 4.00, 0.00 is in use and this deployment asks for up to 4.50");
    }

    #[test]
    fn redeploy_does_not_count_its_own_containers() {
        let running = [running("b", 3.0, 7 * GIB)];
        assert_eq!(admit(pool(), &running, 0, "b", &[request(3.0, 7 * GIB, 1)]), Ok(()));
    }

    #[test]
    fn unlimited_containers_reserve_nothing() {
        let running = [running("a", 0.0, 0), running("c", 2.0, 4 * GIB), running("d", 0.0, 0)];
        let requests = [request(0.0, 0, 3), request(2.0, 4 * GIB, 1)];
        assert_eq!(admit(pool(), &running, 0, "b", &requests), Ok(()));
        let err = admit(pool(), &running, 0, "b", &[request(0.0, 0, 5), request(2.5, GIB, 1)]).unwrap_err();
        assert_eq!(err, "CPU pool is 4.00, 2.00 is in use and this deployment asks for up to 2.50");
    }

    #[test]
    fn storage_full_is_refused() {
        let err = admit(pool(), &[], 100 * GIB, "b", &[request(0.1, GIB, 1)]).unwrap_err();
        assert_eq!(err, "storage pool is 100.00 GiB, 100.00 GiB is already used");
    }

    #[test]
    fn no_storage_pool_means_no_storage_check() {
        let pool = Pool { storage_bytes: None, ..pool() };
        assert_eq!(admit(pool, &[], u64::MAX, "b", &[request(0.1, GIB, 1)]), Ok(()));
    }
}
