use crate::AsyncPollFn;

type Task = (AsyncPollFn, i64);

#[derive(Debug)]
pub(crate) enum TaskStack {
    Inline(Option<Task>),
    Nested(Vec<Task>),
}

impl Default for TaskStack {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskStack {
    pub(crate) const fn new() -> Self {
        Self::Inline(None)
    }

    pub(crate) fn single(poll: AsyncPollFn, future: i64) -> Self {
        Self::Inline(Some((poll, future)))
    }

    pub(crate) fn push(&mut self, task: Task) {
        match self {
            Self::Inline(Some(root)) => *self = Self::Nested(vec![*root, task]),
            Self::Inline(slot @ None) => *slot = Some(task),
            Self::Nested(tasks) => tasks.push(task),
        }
    }

    pub(crate) fn pop(&mut self) -> Option<Task> {
        match self {
            Self::Inline(task) => task.take(),
            Self::Nested(tasks) => tasks.pop(),
        }
    }

    pub(crate) fn last(&self) -> Option<&Task> {
        match self {
            Self::Inline(task) => task.as_ref(),
            Self::Nested(tasks) => tasks.last(),
        }
    }

    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Inline(task) => usize::from(task.is_some()),
            Self::Nested(tasks) => tasks.len(),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    extern "C" fn completed(_: i64) -> i8 {
        1
    }

    #[test]
    fn one_task_is_inline_even_with_a_null_future() {
        let mut stack = TaskStack::single(completed, 0);
        assert!(matches!(stack, TaskStack::Inline(Some(_))));
        assert_eq!(stack.len(), 1);
        assert_eq!(stack.last().map(|task| task.1), Some(0));
        assert_eq!(stack.pop().map(|task| task.1), Some(0));
        assert!(stack.is_empty());
        stack.push((completed, 7));
        assert!(matches!(stack, TaskStack::Inline(Some(_))));
    }

    #[test]
    fn nesting_preserves_lifo_order_and_reuses_its_allocation() {
        let mut stack = TaskStack::single(completed, 1);
        stack.push((completed, 2));
        let capacity = match &stack {
            TaskStack::Nested(tasks) => tasks.capacity(),
            _ => panic!("nested stack must use a vector"),
        };
        assert_eq!(stack.pop().map(|task| task.1), Some(2));
        stack.push((completed, 3));
        assert!(matches!(&stack, TaskStack::Nested(tasks) if tasks.capacity() == capacity));
        assert_eq!(stack.pop().map(|task| task.1), Some(3));
        assert_eq!(stack.pop().map(|task| task.1), Some(1));
        assert!(stack.is_empty());
        assert!(stack.pop().is_none());
        assert!(matches!(&stack, TaskStack::Nested(tasks) if tasks.capacity() == capacity));
    }

    #[test]
    fn push_pop_and_move_match_a_vector_stack() {
        let mut stack = TaskStack::new();
        let mut oracle = Vec::new();
        for value in 0..1000 {
            if value % 3 == 0 {
                assert_eq!(stack.pop().map(|task| task.1), oracle.pop());
            } else {
                stack.push((completed, value));
                oracle.push(value);
            }
            if value % 17 == 0 {
                let saved = std::mem::take(&mut stack);
                assert!(stack.is_empty());
                stack = saved;
            }
            assert_eq!(stack.len(), oracle.len());
            assert_eq!(stack.last().map(|task| task.1), oracle.last().copied());
        }
        while let Some(expected) = oracle.pop() {
            assert_eq!(stack.pop().map(|task| task.1), Some(expected));
        }
        assert!(stack.is_empty());
    }
}
