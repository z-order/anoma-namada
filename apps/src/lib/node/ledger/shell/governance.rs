use anoma::ledger::governance::storage as gov_storage;
use anoma::ledger::governance::utils::{
    compute_tally, get_proposal_votes, ProposalEvent,
};
use anoma::ledger::governance::vp::ADDRESS as gov_address;
use anoma::ledger::storage::types::encode;
use anoma::ledger::storage::{DBIter, StorageHasher, DB};
use anoma::ledger::treasury::ADDRESS as treasury_address;
use anoma::types::address::{xan as m1t, Address};
use anoma::types::governance::TallyResult;
use anoma::types::storage::Epoch;
use anoma::types::token;

use super::*;
use crate::node::ledger::events::EventType;

pub struct ProposalsResult {
    passed: Vec<u64>,
    rejected: Vec<u64>,
}

pub fn execute_governance_proposals<D, H>(
    shell: &mut Shell<D, H>,
    new_epoch: bool,
    response: &mut shim::response::FinalizeBlock,
) -> Result<ProposalsResult>
where
    D: DB + for<'iter> DBIter<'iter> + Sync + 'static,
    H: StorageHasher + Sync + 'static,
{
    let mut proposals_result = ProposalsResult {
        passed: Vec::new(),
        rejected: Vec::new(),
    };

    if !new_epoch {
        return Ok(proposals_result);
    }

    for id in std::mem::take(&mut shell.proposal_data) {
        let proposal_funds_key = gov_storage::get_funds_key(id);
        let proposal_start_epoch_key =
            gov_storage::get_voting_start_epoch_key(id);

        let funds = shell
            .read_storage_key::<token::Amount>(&proposal_funds_key)
            .ok_or_else(|| {
                Error::BadProposal(id, "Invalid proposal funds.".to_string())
            })?;
        let proposal_start_epoch = shell
            .read_storage_key::<Epoch>(&proposal_start_epoch_key)
            .ok_or_else(|| {
                Error::BadProposal(
                    id,
                    "Invalid proposal start_epoch.".to_string(),
                )
            })?;

        let votes =
            get_proposal_votes(&shell.storage, proposal_start_epoch, id);
        let tally_result =
            compute_tally(&shell.storage, proposal_start_epoch, votes);

        let transfer_address = match tally_result {
            TallyResult::Passed => {
                let proposal_author_key = gov_storage::get_author_key(id);
                let proposal_author = shell
                    .read_storage_key::<Address>(&proposal_author_key)
                    .ok_or_else(|| {
                        Error::BadProposal(
                            id,
                            "Invalid proposal author.".to_string(),
                        )
                    })?;

                let proposal_code_key = gov_storage::get_proposal_code_key(id);
                let proposal_code =
                    shell.read_storage_key_bytes(&proposal_code_key);
                match proposal_code {
                    Some(proposal_code) => {
                        let tx = Tx::new(proposal_code, Some(encode(&id)));
                        let tx_type =
                            TxType::Decrypted(DecryptedTx::Decrypted(tx));
                        let pending_execution_key =
                            gov_storage::get_proposal_execution_key(id);
                        shell
                            .storage
                            .write(&pending_execution_key, "")
                            .expect("Should be able to write to storage.");
                        let tx_result = protocol::apply_tx(
                            tx_type,
                            0, /*  this is used to compute the fee
                                * based on the code size. We dont
                                * need it here. */
                            &mut BlockGasMeter::default(),
                            &mut shell.write_log,
                            &shell.storage,
                            &mut shell.vp_wasm_cache,
                            &mut shell.tx_wasm_cache,
                        );
                        shell
                            .storage
                            .delete(&pending_execution_key)
                            .expect("Should be able to delete the storage.");
                        match tx_result {
                            Ok(tx_result) => {
                                if tx_result.is_accepted() {
                                    shell.write_log.commit_tx();
                                    let proposal_event: Event =
                                        ProposalEvent::new(
                                            EventType::Proposal.to_string(),
                                            TallyResult::Passed,
                                            id,
                                            true,
                                            true,
                                        )
                                        .into();
                                    response.events.push(proposal_event);
                                    proposals_result.passed.push(id);

                                    proposal_author
                                } else {
                                    shell.write_log.drop_tx();
                                    let proposal_event: Event =
                                        ProposalEvent::new(
                                            EventType::Proposal.to_string(),
                                            TallyResult::Passed,
                                            id,
                                            true,
                                            false,
                                        )
                                        .into();
                                    response.events.push(proposal_event);
                                    proposals_result.rejected.push(id);

                                    treasury_address
                                }
                            }
                            Err(_e) => {
                                shell.write_log.drop_tx();
                                let proposal_event: Event = ProposalEvent::new(
                                    EventType::Proposal.to_string(),
                                    TallyResult::Passed,
                                    id,
                                    true,
                                    false,
                                )
                                .into();
                                response.events.push(proposal_event);
                                proposals_result.rejected.push(id);

                                treasury_address
                            }
                        }
                    }
                    None => {
                        let proposal_event: Event = ProposalEvent::new(
                            EventType::Proposal.to_string(),
                            TallyResult::Passed,
                            id,
                            false,
                            false,
                        )
                        .into();
                        response.events.push(proposal_event);
                        proposals_result.passed.push(id);

                        proposal_author
                    }
                }
            }
            TallyResult::Rejected | TallyResult::Unknown => {
                let proposal_event: Event = ProposalEvent::new(
                    EventType::Proposal.to_string(),
                    TallyResult::Rejected,
                    id,
                    false,
                    false,
                )
                .into();
                response.events.push(proposal_event);
                proposals_result.rejected.push(id);

                treasury_address
            }
        };

        // transfer proposal locked funds
        shell
            .storage
            .transfer(&m1t(), funds, &gov_address, &transfer_address);
    }

    Ok(proposals_result)
}
