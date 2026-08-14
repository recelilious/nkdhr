//! Stable global workspace numbers with output-group-local activation.

use std::collections::BTreeMap;
use std::fmt;

/// User-facing workspace numbers. Keyboard slots expose 1 through 9 and use
/// the `0` key for workspace 10; additional output groups may still receive a
/// stable number above 10 even though no default digit binding addresses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorkspaceId(u16);

impl WorkspaceId {
    pub const FIRST: Self = Self(1);
    pub const LAST_KEYBOARD: Self = Self(10);

    pub fn new(number: u16) -> Result<Self, WorkspaceError> {
        if number == 0 {
            Err(WorkspaceError::InvalidNumber)
        } else {
            Ok(Self(number))
        }
    }

    pub fn from_digit_key(digit: u8) -> Result<Self, WorkspaceError> {
        match digit {
            1..=9 => Ok(Self(u16::from(digit))),
            0 => Ok(Self::LAST_KEYBOARD),
            _ => Err(WorkspaceError::InvalidDigit),
        }
    }

    pub fn number(self) -> u16 {
        self.0
    }

    pub fn canvas_name(self) -> String {
        format!("workspace:{}", self.0)
    }
}

impl fmt::Display for WorkspaceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceSwitch {
    Noop {
        group: String,
        workspace: WorkspaceId,
    },
    Activate {
        group: String,
        previous: WorkspaceId,
        workspace: WorkspaceId,
    },
    Swap {
        group: String,
        previous: WorkspaceId,
        workspace: WorkspaceId,
        other_group: String,
    },
}

/// Global numbers remain unique while each output group has one independent
/// active workspace. Asking one group for a workspace currently visible on a
/// different group swaps the two views; this avoids cloning one live Wayland
/// surface tree onto two independently active desktops.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceAssignments {
    by_group: BTreeMap<String, WorkspaceId>,
    by_workspace: BTreeMap<WorkspaceId, String>,
}

impl WorkspaceAssignments {
    pub fn workspace_for_group(&self, group: &str) -> Option<WorkspaceId> {
        self.by_group.get(group).copied()
    }

    pub fn owner_of(&self, workspace: WorkspaceId) -> Option<&str> {
        self.by_workspace.get(&workspace).map(String::as_str)
    }

    pub fn ensure_group(&mut self, group: impl Into<String>) -> WorkspaceId {
        let group = group.into();
        if let Some(workspace) = self.by_group.get(&group) {
            return *workspace;
        }
        let workspace = self.first_unowned();
        self.by_group.insert(group.clone(), workspace);
        self.by_workspace.insert(workspace, group);
        workspace
    }

    pub fn switch(
        &mut self,
        group: &str,
        workspace: WorkspaceId,
    ) -> Result<WorkspaceSwitch, WorkspaceError> {
        let previous = self
            .workspace_for_group(group)
            .ok_or(WorkspaceError::UnknownGroup)?;
        if previous == workspace {
            return Ok(WorkspaceSwitch::Noop {
                group: group.to_owned(),
                workspace,
            });
        }

        if let Some(other_group) = self.by_workspace.get(&workspace).cloned() {
            self.by_group.insert(group.to_owned(), workspace);
            self.by_group.insert(other_group.clone(), previous);
            self.by_workspace.insert(workspace, group.to_owned());
            self.by_workspace.insert(previous, other_group.clone());
            Ok(WorkspaceSwitch::Swap {
                group: group.to_owned(),
                previous,
                workspace,
                other_group,
            })
        } else {
            self.by_group.insert(group.to_owned(), workspace);
            self.by_workspace.remove(&previous);
            self.by_workspace.insert(workspace, group.to_owned());
            Ok(WorkspaceSwitch::Activate {
                group: group.to_owned(),
                previous,
                workspace,
            })
        }
    }

    pub fn groups(&self) -> impl Iterator<Item = (&str, WorkspaceId)> {
        self.by_group
            .iter()
            .map(|(group, workspace)| (group.as_str(), *workspace))
    }

    fn first_unowned(&self) -> WorkspaceId {
        let mut number = 1_u16;
        loop {
            let workspace = WorkspaceId(number);
            if !self.by_workspace.contains_key(&workspace) {
                return workspace;
            }
            number = number
                .checked_add(1)
                .expect("the number of output groups cannot exhaust u16 workspaces");
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceError {
    InvalidNumber,
    InvalidDigit,
    UnknownGroup,
}

impl fmt::Display for WorkspaceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidNumber => "workspace number must be positive",
            Self::InvalidDigit => "workspace digit must be between 0 and 9",
            Self::UnknownGroup => "workspace switch target group is unknown",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for WorkspaceError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_displays_take_low_numbers_and_new_workspace_follows_local_focus() {
        let mut assignments = WorkspaceAssignments::default();
        assert_eq!(
            assignments.ensure_group("left"),
            WorkspaceId::new(1).unwrap()
        );
        assert_eq!(
            assignments.ensure_group("right"),
            WorkspaceId::new(2).unwrap()
        );
        let third = WorkspaceId::new(3).unwrap();
        assert_eq!(
            assignments.switch("right", third).unwrap(),
            WorkspaceSwitch::Activate {
                group: "right".into(),
                previous: WorkspaceId::new(2).unwrap(),
                workspace: third,
            }
        );
        assert_eq!(assignments.owner_of(third), Some("right"));
        assert_eq!(assignments.owner_of(WorkspaceId::new(2).unwrap()), None);
    }

    #[test]
    fn requesting_a_visible_workspace_swaps_independent_group_activity() {
        let mut assignments = WorkspaceAssignments::default();
        let first = assignments.ensure_group("left");
        let second = assignments.ensure_group("right");
        assert_eq!(
            assignments.switch("left", second).unwrap(),
            WorkspaceSwitch::Swap {
                group: "left".into(),
                previous: first,
                workspace: second,
                other_group: "right".into(),
            }
        );
        assert_eq!(assignments.workspace_for_group("left"), Some(second));
        assert_eq!(assignments.workspace_for_group("right"), Some(first));
    }

    #[test]
    fn reconnecting_a_named_group_preserves_its_workspace_number() {
        let mut assignments = WorkspaceAssignments::default();
        let original = assignments.ensure_group("external");
        assert_eq!(assignments.ensure_group("external"), original);
        assert_eq!(WorkspaceId::from_digit_key(0).unwrap().number(), 10);
        assert!(WorkspaceId::from_digit_key(11).is_err());
    }
}
