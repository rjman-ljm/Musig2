#![allow(dead_code)]
use std::fmt::Debug;

use crate::cli::party::traits::state_machine::*;

use crate::cli::party::sim::benchmark::Benchmark;
pub use crate::cli::party::sim::benchmark::{BenchmarkResults, Measurements};

/// Emulates running protocol between local parties
///
/// Takes parties (every party is instance of [StateMachine](crate::sm::StateMachine)) and
/// executes protocol between them. It logs whole process (changing state of party, receiving
/// messages, etc.) in stdout.
///
/// Compared to [AsyncSimulation](super::AsyncSimulation), it's lightweight (doesn't require
/// async runtime), and, more importantly, executes everything in straight order (sequently, without
/// any parallelism). It makes this sumaltion more useful for writing benchmarks that detect
/// performance regression.
///
/// ## Limitations
/// * No proper error handling. It should attach a context to returning error (like current round,
///   what we was doing when error occurred, etc.). The only way to determine error context is to
///   look at stdout and find out what happened from logs.
/// * Logs everything to stdout. No choice.
///
/// ## Example
/// ```no_run
/// # use round_based::StateMachine;
/// # use round_based::dev::Simulation;
/// # trait Builder { fn new(party_i: u16, party_n: u16) -> Self; }
/// # fn is_valid<T>(_: &T) -> bool { true }
/// # fn _test<Party: StateMachine + Builder>() -> Result<(), Party::Err>
/// # where Party: std::fmt::Debug,
/// #       Party::Err: std::fmt::Debug,
/// #       Party::MessageBody: std::fmt::Debug + Clone,
/// # {
/// let results = Simulation::new()
///     .add_party(Party::new(1, 3))
///     .add_party(Party::new(2, 3))
///     .add_party(Party::new(3, 3))
///     .run()?;
/// assert!(results.into_iter().all(|r| is_valid(&r)));
/// # Ok(())
/// # }
/// ```
pub struct Simulation<P> {
    /// Parties running a protocol
    ///
    /// Field is exposed mainly to allow examining parties state after simulation is completed.
    pub parties: Vec<P>,
    benchmark: Benchmark,
}

impl<P> Simulation<P> {
    /// Creates new simulation
    pub fn new() -> Self {
        Self {
            parties: vec![],
            benchmark: Benchmark::disabled(),
        }
    }

    /// Adds protocol participant
    pub fn add_party(&mut self, party: P) -> &mut Self {
        self.parties.push(party);
        self
    }

    /// Enables benchmarks so they can be [retrieved](Simulation::benchmark_results) after simulation
    /// is completed
    pub fn enable_benchmarks(&mut self, enable: bool) -> &mut Self {
        if enable {
            self.benchmark = Benchmark::enabled()
        } else {
            self.benchmark = Benchmark::disabled()
        }
        self
    }

    /// Returns benchmark results if they were [enabled](Simulation::enable_benchmarks)
    ///
    /// Benchmarks show how much time (in average) [proceed](StateMachine::proceed) method takes for
    /// proceeding particular rounds. Benchmarks might help to find out which rounds are cheap to
    /// proceed, and which of them are expensive to compute.
    pub fn benchmark_results(&self) -> Option<&BenchmarkResults> {
        self.benchmark.results()
    }
}

impl<P> Simulation<P>
where
    P: StateMachine,
    P: Debug,
    P::Err: Debug,
    P::MessageBody: Debug + Clone,
{
    /// Runs a simulation
    ///
    /// ## Returns
    /// Returns either Vec of protocol outputs (one output for each one party) or first
    /// occurred critical error.
    ///
    /// ## Panics
    /// * Number of parties is less than 2
    pub fn run(&mut self) -> Result<Vec<P::Output>, P::Err> {
        assert!(self.parties.len() >= 2, "at least two parties required");

        let mut parties: Vec<_> = self
            .parties
            .iter_mut()
            .map(|p| Party { state: p })
            .collect();

        println!("Simulation starts");

        let mut msgs_pull = vec![];

        for party in &mut parties {
            party.proceed_if_needed(&mut self.benchmark)?;
            party.send_outgoing(&mut msgs_pull);
        }

        if let Some(results) = finish_if_possible(&mut parties)? {
            return Ok(results);
        }

        loop {
            let msgs_pull_frozen = msgs_pull.split_off(0);

            for party in &mut parties {
                party.handle_incoming(&msgs_pull_frozen)?;
                party.send_outgoing(&mut msgs_pull);
            }

            for party in &mut parties {
                party.proceed_if_needed(&mut self.benchmark)?;
                party.send_outgoing(&mut msgs_pull);
            }

            if let Some(results) = finish_if_possible(&mut parties)? {
                return Ok(results);
            }
        }
    }
}

struct Party<'p, P> {
    state: &'p mut P,
}

impl<'p, P> Party<'p, P>
where
    P: StateMachine,
    P: Debug,
    P::Err: Debug,
    P::MessageBody: Debug + Clone,
{
    pub fn proceed_if_needed(&mut self, benchmark: &mut Benchmark) -> Result<(), P::Err> {
        if !self.state.wants_to_proceed() {
            return Ok(());
        }

        println!("Party {} wants to proceed", self.state.party_ind());
        println!("  - before: {:?}", self.state);

        let round_old = self.state.current_round();
        let stopwatch = benchmark.start();
        match self.state.proceed() {
            Ok(()) => (),
            Err(err) if err.is_critical() => return Err(err),
            Err(err) => {
                println!("Non-critical error encountered: {:?}", err);
            }
        }
        let round_new = self.state.current_round();
        let duration = if round_old != round_new {
            Some(stopwatch.stop_and_save(round_old))
        } else {
            None
        };

        println!("  - after : {:?}", self.state);
        println!("  - took  : {:?}", duration);
        println!();

        Ok(())
    }

    pub fn send_outgoing(&mut self, msgs_pull: &mut Vec<Msg<P::MessageBody>>) {
        if !self.state.message_queue().is_empty() {
            println!(
                "Party {} sends {} message(s)",
                self.state.party_ind(),
                self.state.message_queue().len()
            );
            println!();

            msgs_pull.append(self.state.message_queue())
        }
    }

    pub fn handle_incoming(&mut self, msgs_pull: &[Msg<P::MessageBody>]) -> Result<(), P::Err> {
        for msg in msgs_pull {
            if Some(self.state.party_ind()) != msg.receiver
                && (msg.receiver.is_some() || msg.sender == self.state.party_ind())
            {
                continue;
            }
            println!(
                "Party {} got message from={}, broadcast={}: {:?}",
                self.state.party_ind(),
                msg.sender,
                msg.receiver.is_none(),
                msg.body,
            );
            println!("  - before: {:?}", self.state);
            match self.state.handle_incoming(msg.clone()) {
                Ok(()) => (),
                Err(err) if err.is_critical() => return Err(err),
                Err(err) => {
                    println!("Non-critical error encountered: {:?}", err);
                }
            }
            println!("  - after : {:?}", self.state);
            println!();
        }
        Ok(())
    }
}

fn finish_if_possible<P>(parties: &mut Vec<Party<P>>) -> Result<Option<Vec<P::Output>>, P::Err>
where
    P: StateMachine,
    P: Debug,
    P::Err: Debug,
    P::MessageBody: Debug + Clone,
{
    let someone_is_finished = parties.iter().any(|p| p.state.is_finished());
    if !someone_is_finished {
        return Ok(None);
    }

    let everyone_are_finished = parties.iter().all(|p| p.state.is_finished());
    if everyone_are_finished {
        let mut results = vec![];
        for party in parties {
            results.push(
                party
                    .state
                    .pick_output()
                    .expect("is_finished == true, but pick_output == None")?,
            )
        }

        println!("Simulation is finished");
        println!();

        Ok(Some(results))
    } else {
        let finished: Vec<_> = parties
            .iter()
            .filter(|p| p.state.is_finished())
            .map(|p| p.state.party_ind())
            .collect();
        let not_finished: Vec<_> = parties
            .iter()
            .filter(|p| !p.state.is_finished())
            .map(|p| p.state.party_ind())
            .collect();

        println!("Warning: some of parties have finished the protocol, but other parties have not");
        println!("Finished parties:     {:?}", finished);
        println!("Not finished parties: {:?}", not_finished);
        println!();

        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use crate::cli::node::format_musig_msg;
    use crate::cli::party::{musig2_instance::Musig2Instance, sim::simulation::Simulation};
    use crate::cli::protocals::KeyPair;

    #[test]
    fn simulate_musig_protocol() {
        let message = format_musig_msg(Vec::from("test".as_bytes()));

        let kp1 = KeyPair::create().unwrap();
        let kp2 = KeyPair::create().unwrap();
        let kp3 = KeyPair::create().unwrap();

        let mut simulation = Simulation::new();
        simulation
            .enable_benchmarks(true)
            .add_party(Musig2Instance::with_fixed_seed(1, 3, message.clone(), kp1))
            .add_party(Musig2Instance::with_fixed_seed(2, 3, message.clone(), kp2))
            .add_party(Musig2Instance::with_fixed_seed(3, 3, message.clone(), kp3));
        let result = simulation.run().expect("simulation failed");
        println!("sign result:{:?}", result[0]);
        println!("Benchmarks:");
        println!("{:#?}", simulation.benchmark_results().unwrap());
    }
}
