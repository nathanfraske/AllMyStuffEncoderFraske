use allmystuff_byte_queues::ByteQueues;

#[test]
fn oversized_chunks_preserve_drop_and_notification_policy() {
    let queues = ByteQueues::new(4);
    queues.ensure("viewer");
    assert!(queues.enqueue("viewer", vec![1]));

    // Overflow removes both the old chunk and the oversized new one.
    // This append began with a nonempty queue, so it does not notify.
    assert!(!queues.enqueue("viewer", vec![2; 5]));
    assert!(queues.poll("viewer").is_empty());

    // An empty queue still signals a poll even if eviction empties it again.
    assert!(queues.enqueue("viewer", vec![3; 5]));
    assert!(queues.poll("viewer").is_empty());

    assert!(queues.enqueue("viewer", vec![4]));
    assert_eq!(queues.poll("viewer"), [1, 0, 0, 0, 4]);
}
