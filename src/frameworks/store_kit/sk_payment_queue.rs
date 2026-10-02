/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0.
 * If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `SKPaymentQueue` — StoreKit in-app purchase queue.
//!
//! With IAP emulation enabled (Cheat Engine `IAP` toggle or the
//! `TOUCHHLE_IAP_EMULATION` environment variable), `addPayment:` completes the
//! transaction locally as `SKPaymentTransactionStatePurchased`, so games that
//! gate content behind StoreKit unlock it without any App Store contact.
//! The emulator never contacts Apple or creates a genuine signed receipt.
//! Apps may still make their usual network calls with the synthetic receipt.
//!
//! With emulation disabled (Cheat Engine inactive — the default), every
//! entry point behaves exactly like the original pre-emulation stubs:
//! `canMakePayments` is false, `addPayment:` notifies the observer with an
//! empty transaction array, and nothing is fabricated — no transactions,
//! no receipts, no product responses.

use crate::frameworks::foundation::{ns_string, NSInteger, NSUInteger};
use crate::frameworks::store_kit::{
    emulation_enabled, SK_PAYMENT_TRANSACTION_STATE_PURCHASED,
    SK_PAYMENT_TRANSACTION_STATE_RESTORED,
};
use crate::objc::{
    autorelease, id, msg, msg_class, nil, objc_classes, release, retain, ClassExports, HostObject,
    NSZonePtr,
};
use crate::Environment;

const PURCHASE_PRODUCT_IDENTIFIERS_KEY: &str = "touchHLE.IAP.EmulatedProductIdentifiers.v1";
const PURCHASE_TRANSACTION_IDENTIFIERS_KEY: &str = "touchHLE.IAP.EmulatedTransactionIdentifiers.v1";

// MARK: - Per-process state

#[derive(Clone, Debug, Eq, PartialEq)]
struct PurchaseRecord {
    product_identifier: String,
    transaction_identifier: String,
}

/// Singleton cache and local purchase history for `[SKPaymentQueue defaultQueue]`.
#[derive(Default)]
pub struct State {
    default_queue: Option<id>,
    transaction_counter: u64,
    purchase_history_loaded: bool,
    purchased: Vec<PurchaseRecord>,
}

impl State {
    fn get(env: &mut Environment) -> &mut State {
        &mut env.framework_state.store_kit.payment_queue
    }
}

#[derive(Default)]
struct SKPaymentQueueHostObject {
    /// SKPaymentTransactionObserver — weak reference
    observer: id,
    /// Transactions delivered but not yet finished. Each is retained.
    pending: Vec<id>,
}
impl HostObject for SKPaymentQueueHostObject {}

/// Generate a fresh, process-unique identifier for an emulated transaction.
fn next_transaction_identifier(env: &mut Environment, prefix: &str) -> (String, id) {
    let number = {
        let state = State::get(env);
        state.transaction_counter += 1;
        state.transaction_counter
    };
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let identifier = format!(
        "touchHLE.{}.{}.{}.{}",
        env.bundle.bundle_identifier(),
        prefix,
        timestamp,
        number
    );
    let object = ns_string::from_rust_string(env, identifier.clone());
    (identifier, object)
}

/// Read a guest NSString as an owned Rust string (empty if nil).
fn identifier_string(env: &mut Environment, string: id) -> String {
    if string == nil {
        return String::new();
    }
    ns_string::to_rust_string(env, string).into_owned()
}

fn purchase_history_from_identifiers(
    product_identifiers: &[String],
    transaction_identifiers: &[String],
) -> Vec<PurchaseRecord> {
    let mut purchases = Vec::new();
    for (product_identifier, transaction_identifier) in
        product_identifiers.iter().zip(transaction_identifiers)
    {
        remember_purchase_record(&mut purchases, product_identifier, transaction_identifier);
    }
    purchases
}

fn remember_purchase_record(
    purchases: &mut Vec<PurchaseRecord>,
    product_identifier: &str,
    transaction_identifier: &str,
) -> bool {
    if product_identifier.is_empty()
        || transaction_identifier.is_empty()
        || purchases
            .iter()
            .any(|purchase| purchase.product_identifier == product_identifier)
    {
        return false;
    }
    purchases.push(PurchaseRecord {
        product_identifier: product_identifier.to_owned(),
        transaction_identifier: transaction_identifier.to_owned(),
    });
    true
}

fn load_purchase_history(env: &mut Environment) -> Vec<PurchaseRecord> {
    let defaults: id = msg_class![env; NSUserDefaults standardUserDefaults];
    let product_key = ns_string::get_static_str(env, PURCHASE_PRODUCT_IDENTIFIERS_KEY);
    let transaction_key = ns_string::get_static_str(env, PURCHASE_TRANSACTION_IDENTIFIERS_KEY);
    let products: id = msg![env; defaults stringArrayForKey:product_key];
    let transactions: id = msg![env; defaults stringArrayForKey:transaction_key];
    let product_count: NSUInteger = msg![env; products count];
    let transaction_count: NSUInteger = msg![env; transactions count];
    if product_count != transaction_count {
        log!(
            "SKPaymentQueue: saved local purchase history has mismatched product/transaction counts ({} vs {}); keeping complete pairs only.",
            product_count,
            transaction_count
        );
    }
    let mut product_identifiers = Vec::with_capacity(product_count as usize);
    let mut transaction_identifiers = Vec::with_capacity(transaction_count as usize);
    for index in 0..product_count {
        let identifier: id = msg![env; products objectAtIndex:index];
        product_identifiers.push(identifier_string(env, identifier));
    }
    for index in 0..transaction_count {
        let identifier: id = msg![env; transactions objectAtIndex:index];
        transaction_identifiers.push(identifier_string(env, identifier));
    }
    purchase_history_from_identifiers(&product_identifiers, &transaction_identifiers)
}

fn ensure_purchase_history_loaded(env: &mut Environment) {
    if State::get(env).purchase_history_loaded {
        return;
    }
    State::get(env).purchase_history_loaded = true;
    let purchases = load_purchase_history(env);
    State::get(env).purchased = purchases;
}

fn persist_purchase_history(env: &mut Environment) {
    let purchases = State::get(env).purchased.clone();
    let defaults: id = msg_class![env; NSUserDefaults standardUserDefaults];
    let products: id = msg_class![env; NSMutableArray array];
    let transactions: id = msg_class![env; NSMutableArray array];
    for purchase in purchases {
        let product_identifier = ns_string::from_rust_string(env, purchase.product_identifier);
        let transaction_identifier =
            ns_string::from_rust_string(env, purchase.transaction_identifier);
        () = msg![env; products addObject:product_identifier];
        () = msg![env; transactions addObject:transaction_identifier];
        release(env, product_identifier);
        release(env, transaction_identifier);
    }
    let product_key = ns_string::get_static_str(env, PURCHASE_PRODUCT_IDENTIFIERS_KEY);
    let transaction_key = ns_string::get_static_str(env, PURCHASE_TRANSACTION_IDENTIFIERS_KEY);
    () = msg![env; defaults setObject:products forKey:product_key];
    () = msg![env; defaults setObject:transactions forKey:transaction_key];
    let saved: bool = msg![env; defaults synchronize];
    if !saved {
        log!("Warning: failed to persist local StoreKit purchase history.");
    }
}

/// Fields of an `SKPaymentTransaction`. Retained on assignment.
#[derive(Default)]
struct SKPaymentTransactionHostObject {
    state: NSInteger,
    transaction_identifier: id,
    payment: id,
    original_transaction: id,
}
impl HostObject for SKPaymentTransactionHostObject {}

/// Create a transaction. Returns it at the `alloc` refcount of 1; the caller
/// owns that reference and must balance it (autorelease for transient use, or
/// a pending-queue reference).
fn make_transaction(
    env: &mut Environment,
    state: NSInteger,
    payment: id,
    transaction_identifier: id,
    original_transaction: id,
) -> id {
    let transaction: id = msg_class![env; SKPaymentTransaction alloc];
    {
        let host = env
            .objc
            .borrow_mut::<SKPaymentTransactionHostObject>(transaction);
        host.state = state;
        host.transaction_identifier = transaction_identifier;
        host.payment = payment;
        host.original_transaction = original_transaction;
    }
    retain(env, transaction_identifier);
    if payment != nil {
        retain(env, payment);
    }
    if original_transaction != nil {
        retain(env, original_transaction);
    }
    transaction
}

/// Deliver `transactions` to the queue's observer via
/// `paymentQueue:updatedTransactions:`.
fn deliver_transactions(env: &mut Environment, queue: id, transactions: &[id]) {
    let observer = env.objc.borrow::<SKPaymentQueueHostObject>(queue).observer;
    if observer == nil || transactions.is_empty() {
        return;
    }
    let array: id = msg_class![env; NSMutableArray array];
    for &transaction in transactions {
        () = msg![env; array addObject:transaction];
    }
    let sel = env.objc.register_host_selector(
        "paymentQueue:updatedTransactions:".to_string(),
        &mut env.mem,
    );
    let responds: bool = msg![env; observer respondsToSelector:sel];
    if responds {
        () = msg![env; observer paymentQueue:queue updatedTransactions:array];
    } else {
        log!(
            "Warning: SKPaymentTransactionObserver does not respond to paymentQueue:updatedTransactions:; transaction result dropped."
        );
    }
}

/// Remember a local purchase and persist it for later restore calls.
fn remember_purchase(
    env: &mut Environment,
    product_identifier: &str,
    transaction_identifier: &str,
) {
    ensure_purchase_history_loaded(env);
    if remember_purchase_record(
        &mut State::get(env).purchased,
        product_identifier,
        transaction_identifier,
    ) {
        persist_purchase_history(env);
    }
}

/// Create an autoreleased SKPayment for a Rust product identifier.
fn payment_for_identifier(env: &mut Environment, identifier: &str) -> id {
    let identifier = ns_string::from_rust_string(env, identifier.to_owned());
    autorelease(env, identifier);
    let payment: id = msg_class![env; SKPayment alloc];
    let payment: id = msg![env; payment initWithProductIdentifier:identifier];
    autorelease(env, payment)
}

// MARK: - SKPayment

struct SKPaymentHostObject {
    product_identifier: id,
    application_username: id,
    request_data: id,
    quantity: NSInteger,
    simulates_ask_to_buy_in_sandbox: bool,
}
impl Default for SKPaymentHostObject {
    fn default() -> Self {
        Self {
            product_identifier: nil,
            application_username: nil,
            request_data: nil,
            quantity: 1,
            simulates_ask_to_buy_in_sandbox: false,
        }
    }
}
impl HostObject for SKPaymentHostObject {}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation SKPaymentQueue: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::new(SKPaymentQueueHostObject {
        observer: nil,
        pending: Vec::new(),
    });
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

// MARK: - Singleton

+ (id)defaultQueue {
    // Always return the same singleton so observers are not lost between calls.
    if let Some(queue) = State::get(env).default_queue {
        return queue;
    }
    let queue: id = msg![env; this alloc];
    let queue: id = msg![env; queue init];
    // refcount=1 is our singleton retain — do NOT autorelease
    State::get(env).default_queue = Some(queue);
    log!("SKPaymentQueue defaultQueue: singleton created");
    queue
}

+ (bool)canMakePayments {
    emulation_enabled()
}

// MARK: - Init

- (id)init {
    this
}

- (())dealloc {
    let (observer, pending) = {
        let host = env.objc.borrow::<SKPaymentQueueHostObject>(this);
        (host.observer, host.pending.clone())
    };
    release(env, observer);
    for transaction in pending {
        release(env, transaction);
    }
    env.objc.dealloc_object(this, &mut env.mem)
}

// MARK: - Observers

- (())addTransactionObserver:(id)observer {
    // Убрали .unwrap()
    let host_obj = env.objc.borrow_mut::<SKPaymentQueueHostObject>(this);
    host_obj.observer = observer;
}

- (())removeTransactionObserver:(id)_observer {
    // Убрали .unwrap()
    let host_obj = env.objc.borrow_mut::<SKPaymentQueueHostObject>(this);
    host_obj.observer = nil;
}

// MARK: - Payment requests

- (())addPayment:(id)payment { // SKPayment*
    let identifier_object: id = if payment != nil {
        msg![env; payment productIdentifier]
    } else {
        nil
    };
    let identifier = identifier_string(env, identifier_object);
    if identifier_object != nil {
        release(env, identifier_object);
    }

    if !emulation_enabled() {
        // Cheat Engine inactive: exact pre-emulation stub behavior — notify
        // the observer with an EMPTY transaction array. No transaction
        // object is created, so nothing in the game can see a success.
        log!("SKPaymentQueue addPayment: stubbed — failing transaction immediately");
        let observer = env.objc.borrow::<SKPaymentQueueHostObject>(this).observer;
        if observer == nil {
            return;
        }
        let transactions: id = msg_class![env; NSArray new];
        let sel = env
            .objc
            .register_host_selector("paymentQueue:updatedTransactions:".to_string(), &mut env.mem);
        let responds: bool = msg![env; observer respondsToSelector:sel];
        if responds {
            () = msg![env; observer paymentQueue:this updatedTransactions:transactions];
        }
        return;
    }

    let (transaction_identifier_string, transaction_identifier) =
        next_transaction_identifier(env, "iap");
    remember_purchase(env, &identifier, &transaction_identifier_string);
    let transaction = make_transaction(
        env,
        SK_PAYMENT_TRANSACTION_STATE_PURCHASED,
        payment,
        transaction_identifier,
        nil,
    );
    release(env, transaction_identifier);
    // The pending queue owns one reference; the pool guards the delivery call.
    retain(env, transaction);
    autorelease(env, transaction);
    env.objc
        .borrow_mut::<SKPaymentQueueHostObject>(this)
        .pending
        .push(transaction);
    let count = State::get(env).transaction_counter;
    log!(
        "SKPaymentQueue addPayment: IAP emulation ON; product {:?} auto-purchased (transaction #{}).",
        identifier,
        count
    );
    deliver_transactions(env, this, &[transaction]);
}

- (())restoreCompletedTransactions {
    // Убрали .unwrap()
    if !emulation_enabled() {
        let host_obj = env.objc.borrow::<SKPaymentQueueHostObject>(this);
        let observer = host_obj.observer;

        if observer != nil {
            // Вызываем метод делегата, сообщая, что "восстановление" успешно
            // завершено
            let _: () = msg![env; observer paymentQueueRestoreCompletedTransactionsFinished:this];
        }
        return;
    }
    ensure_purchase_history_loaded(env);
    let purchased = State::get(env).purchased.clone();
    let mut restored: Vec<id> = Vec::new();
    for purchase in &purchased {
        let payment = payment_for_identifier(env, &purchase.product_identifier);
        let original_identifier =
            ns_string::from_rust_string(env, purchase.transaction_identifier.clone());
        let original_transaction = make_transaction(
            env,
            SK_PAYMENT_TRANSACTION_STATE_PURCHASED,
            payment,
            original_identifier,
            nil,
        );
        release(env, original_identifier);
        autorelease(env, original_transaction);
        let (_, transaction_identifier) = next_transaction_identifier(env, "restore");
        let transaction = make_transaction(
            env,
            SK_PAYMENT_TRANSACTION_STATE_RESTORED,
            payment,
            transaction_identifier,
            original_transaction,
        );
        release(env, transaction_identifier);
        // The pending queue owns one reference; the pool guards delivery.
        retain(env, transaction);
        autorelease(env, transaction);
        env.objc
            .borrow_mut::<SKPaymentQueueHostObject>(this)
            .pending
            .push(transaction);
        restored.push(transaction);
    }
    log!(
        "SKPaymentQueue restoreCompletedTransactions: IAP emulation ON; restoring {} purchase(s).",
        restored.len()
    );
    deliver_transactions(env, this, &restored);
    let host_obj = env.objc.borrow::<SKPaymentQueueHostObject>(this);
    let observer = host_obj.observer;
    if observer != nil {
        let sel = env.objc.register_host_selector(
            "paymentQueueRestoreCompletedTransactionsFinished:".to_string(),
            &mut env.mem,
        );
        let responds: bool = msg![env; observer respondsToSelector:sel];
        if responds {
            let _: () =
                msg![env; observer paymentQueueRestoreCompletedTransactionsFinished:this];
        }
    }
}

- (())restoreCompletedTransactionsWithApplicationUsername:(id)_username {
    msg![env; this restoreCompletedTransactions]
}

- (())finishTransaction:(id)transaction { // SKPaymentTransaction*
    let was_pending = {
        let host = env.objc.borrow_mut::<SKPaymentQueueHostObject>(this);
        host.pending
            .iter()
            .position(|&t| t == transaction)
            .map(|pos| host.pending.remove(pos))
            .is_some()
    };
    if was_pending {
        release(env, transaction);
    }
    log_dbg!(
        "SKPaymentQueue finishTransaction: completed (was queued: {}).",
        was_pending
    );
}

// MARK: - Downloads (iOS 6+, always empty)

- (id)transactions {
    let pending = env
        .objc
        .borrow::<SKPaymentQueueHostObject>(this)
        .pending
        .clone();
    let array: id = msg_class![env; NSMutableArray array];
    for transaction in pending {
        () = msg![env; array addObject:transaction];
    }
    array
}

- (())startDownloads:(id)_downloads {
    log!("SKPaymentQueue startDownloads: stubbed");
}

- (())pauseDownloads:(id)_downloads {
    log!("SKPaymentQueue pauseDownloads: stubbed");
}

- (())resumeDownloads:(id)_downloads {
    log!("SKPaymentQueue resumeDownloads: stubbed");
}

- (())cancelDownloads:(id)_downloads {
    log!("SKPaymentQueue cancelDownloads: stubbed");
}

@end

@implementation SKPayment: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    env.objc
        .alloc_object(this, Box::<SKPaymentHostObject>::default(), &mut env.mem)
}

+ (id)paymentWithProductIdentifier:(id)identifier { // NSString*
    let payment: id = msg![env; this alloc];
    let payment: id = msg![env; payment initWithProductIdentifier:identifier];
    autorelease(env, payment)
}

+ (id)paymentWithProduct:(id)product { // SKProduct*
    if product == nil {
        return nil;
    }
    let identifier: id = msg![env; product productIdentifier];
    let payment: id = msg![env; this paymentWithProductIdentifier:identifier];
    release(env, identifier);
    payment
}

- (id)initWithProductIdentifier:(id)identifier {
    let previous = {
        let host = env.objc.borrow_mut::<SKPaymentHostObject>(this);
        let previous = host.product_identifier;
        host.product_identifier = identifier;
        previous
    };
    if identifier != nil {
        retain(env, identifier);
    }
    release(env, previous);
    this
}

- (id)productIdentifier {
    let identifier = env
        .objc
        .borrow::<SKPaymentHostObject>(this)
        .product_identifier;
    if identifier == nil {
        ns_string::get_static_str(env, "")
    } else {
        retain(env, identifier);
        identifier
    }
}

- (NSInteger)quantity {
    env.objc.borrow::<SKPaymentHostObject>(this).quantity
}

- (id)applicationUsername {
    let username = env
        .objc
        .borrow::<SKPaymentHostObject>(this)
        .application_username;
    if username != nil {
        retain(env, username);
    }
    username
}

- (id)requestData {
    let data = env.objc.borrow::<SKPaymentHostObject>(this).request_data;
    if data != nil {
        retain(env, data);
    }
    data
}

- (bool)simulatesAskToBuyInSandbox {
    env.objc
        .borrow::<SKPaymentHostObject>(this)
        .simulates_ask_to_buy_in_sandbox
}

- (())dealloc {
    let (identifier, username, request_data) = {
        let host = env.objc.borrow::<SKPaymentHostObject>(this);
        (
            host.product_identifier,
            host.application_username,
            host.request_data,
        )
    };
    release(env, identifier);
    release(env, username);
    release(env, request_data);
    env.objc.dealloc_object(this, &mut env.mem)
}

@end

@implementation SKMutablePayment: SKPayment

- (())setQuantity:(NSInteger)quantity {
    env.objc
        .borrow_mut::<SKPaymentHostObject>(this)
        .quantity = quantity.max(1);
}

- (())setApplicationUsername:(id)username {
    let previous = {
        let host = env.objc.borrow_mut::<SKPaymentHostObject>(this);
        let previous = host.application_username;
        host.application_username = username;
        previous
    };
    if username != nil {
        retain(env, username);
    }
    release(env, previous);
}

- (())setRequestData:(id)request_data {
    let previous = {
        let host = env.objc.borrow_mut::<SKPaymentHostObject>(this);
        let previous = host.request_data;
        host.request_data = request_data;
        previous
    };
    if request_data != nil {
        retain(env, request_data);
    }
    release(env, previous);
}

- (())setSimulatesAskToBuyInSandbox:(bool)simulates_ask_to_buy {
    env.objc
        .borrow_mut::<SKPaymentHostObject>(this)
        .simulates_ask_to_buy_in_sandbox = simulates_ask_to_buy;
}

@end

// MARK: - SKPaymentTransaction

@implementation SKPaymentTransaction: NSObject

- (NSInteger)transactionState {
    env.objc
        .borrow::<SKPaymentTransactionHostObject>(this)
        .state
}

- (id)transactionIdentifier {
    let identifier = env
        .objc
        .borrow::<SKPaymentTransactionHostObject>(this)
        .transaction_identifier;
    if identifier == nil {
        nil
    } else {
        retain(env, identifier);
        identifier
    }
}

- (id)payment {
    let payment = env
        .objc
        .borrow::<SKPaymentTransactionHostObject>(this)
        .payment;
    if payment == nil {
        nil
    } else {
        retain(env, payment);
        payment
    }
}

- (id)originalTransaction {
    let original = env
        .objc
        .borrow::<SKPaymentTransactionHostObject>(this)
        .original_transaction;
    if original == nil {
        nil
    } else {
        retain(env, original);
        original
    }
}

- (id)error {
    nil
}

- (id)receipt {
    let state = env
        .objc
        .borrow::<SKPaymentTransactionHostObject>(this)
        .state;
    if state != SK_PAYMENT_TRANSACTION_STATE_PURCHASED
        && state != SK_PAYMENT_TRANSACTION_STATE_RESTORED
    {
        // Real StoreKit reports a nil receipt for transactions that have not
        // (successfully) run.
        return nil;
    }
    let identifier_object: id = msg![env; this transactionIdentifier];
    let identifier = identifier_string(env, identifier_object);
    release(env, identifier_object);
    // Games of this era commonly gate crediting on a non-empty receipt
    // (nil-check or length-check, sometimes a local parse, often a POST to
    // their server). We cannot produce a genuinely signed App Store receipt,
    // but a stable opaque blob passes the presence checks, so games credit
    // the purchase instead of silently granting nothing.
    let mut blob = b"touchHLE-IAP-receipt/v1:".to_vec();
    blob.extend_from_slice(identifier.as_bytes());
    let len = blob.len() as NSUInteger;
    let bytes = env.mem.alloc(len);
    env.mem
        .bytes_at_mut(bytes.cast(), len)
        .copy_from_slice(&blob);
    let receipt: id = msg_class![env; NSData dataWithBytes:bytes length:len];
    // dataWithBytes:length: copies the buffer, so free our temporary memory.
    env.mem.free(bytes.cast_void());
    autorelease(env, receipt)
}

- (id)transactionDate {
    msg_class![env; NSDate date]
}

- (())dealloc {
    let (transaction_identifier, payment, original_transaction) = {
        let host = env.objc.borrow::<SKPaymentTransactionHostObject>(this);
        (
            host.transaction_identifier,
            host.payment,
            host.original_transaction,
        )
    };
    release(env, transaction_identifier);
    release(env, payment);
    release(env, original_transaction);
    env.objc.dealloc_object(this, &mut env.mem)
}

@end

};

#[cfg(test)]
mod tests {
    use super::{purchase_history_from_identifiers, remember_purchase_record, PurchaseRecord};

    #[test]
    fn purchase_history_keeps_one_stable_original_transaction_per_product() {
        let products = ["coins.small", "coins.small", "unlock.pro"].map(str::to_owned);
        let transactions = ["tx.first", "tx.second", "tx.pro"].map(str::to_owned);

        assert_eq!(
            purchase_history_from_identifiers(&products, &transactions),
            vec![
                PurchaseRecord {
                    product_identifier: "coins.small".to_owned(),
                    transaction_identifier: "tx.first".to_owned(),
                },
                PurchaseRecord {
                    product_identifier: "unlock.pro".to_owned(),
                    transaction_identifier: "tx.pro".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn purchase_history_ignores_empty_or_unpaired_identifiers() {
        let products = ["", "valid.product", "unpaired.product"].map(str::to_owned);
        let transactions = ["tx.empty", "tx.valid"].map(str::to_owned);
        let mut history = purchase_history_from_identifiers(&products, &transactions);

        assert_eq!(history.len(), 1);
        assert!(!remember_purchase_record(&mut history, "", "tx.invalid"));
        assert!(remember_purchase_record(
            &mut history,
            "new.product",
            "tx.new"
        ));
        assert!(!remember_purchase_record(
            &mut history,
            "valid.product",
            "tx.duplicate"
        ));
        assert_eq!(history.len(), 2);
    }

    #[test]
    fn new_payments_default_to_one_item_without_ask_to_buy() {
        let payment = super::SKPaymentHostObject::default();

        assert_eq!(payment.quantity, 1);
        assert!(!payment.simulates_ask_to_buy_in_sandbox);
    }
}
