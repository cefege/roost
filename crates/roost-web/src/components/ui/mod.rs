//! The rust-ui component layer (rust-ui.com, `github.com/rust-ui/ui`): every
//! surface composes its controls, panels, overlays and glyphs from here.
//! Each file is the rust-ui registry component of the same name, adapted to
//! this crate's rules; styling is Tailwind utilities over the theme bridge in
//! `crates/roost-web/tailwind.css`.

pub mod accordion;
pub mod alert;
pub mod alert_dialog;
pub mod anchored_panel;
pub mod attachment;
pub mod avatar;
pub mod badge;
pub mod bubble;
pub mod button;
pub mod button_group;
pub mod card;
pub mod checkbox;
pub mod chip;
pub mod collapsible;
pub mod command;
pub mod command_input;
pub mod command_item;
pub mod command_keys;
pub mod command_layout;
pub mod context_menu;
pub mod dialog;
pub mod dialog_parts;
pub mod dom;
pub mod drawer;
pub mod dropdown_menu;
pub mod dropdown_menu_sub;
#[cfg(any(target_arch = "wasm32", test))]
mod dropdown_menu_sub_placement;
pub mod empty;
pub mod field;
pub mod focus_scope;
pub mod hover_card;
pub mod icon;
pub mod input;
pub mod input_group;
pub mod input_prompt;
pub mod item;
pub mod kbd;
pub mod label;
pub mod list;
pub mod markdown;
pub mod marker;
pub mod menu_items;
pub mod menu_overlay;
pub mod message;
pub mod metric_tile;
pub mod popover;
pub mod progress;
pub mod radio_button;
pub mod radio_button_group;
pub mod scroll_area;
pub mod section_title;
pub mod select;
pub mod select_listbox;
pub mod select_navigation;
pub mod select_placement;
pub mod separator;
pub mod sheet;
pub mod shimmer;
pub mod sidenav;
pub mod sidenav_layout;
pub mod sidenav_links;
pub mod sidenav_types;
pub mod skeleton;
pub mod slider;
pub mod spinner;
pub mod status;
pub mod switch;
pub mod table;
pub mod tabs;
pub mod text_field;
pub mod textarea;
pub mod toast;
mod toast_queue;
pub mod toggle_group;
pub mod tooltip;
pub mod usage_ring;
pub use accordion::{
    Accordion, AccordionContent, AccordionDescription, AccordionHeader, AccordionItem,
    AccordionLink, AccordionTitle, AccordionTrigger,
};
pub use alert::{Alert, AlertDescription, AlertTitle, AlertVariant};
pub use alert_dialog::{
    AlertDialog, AlertDialogAction, AlertDialogBody, AlertDialogCancel, AlertDialogContent,
    AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle,
    AlertDialogTrigger,
};
pub use attachment::{
    Attachment, AttachmentAction, AttachmentActions, AttachmentContent, AttachmentDescription,
    AttachmentGroup, AttachmentMedia, AttachmentMediaVariant, AttachmentOrientation,
    AttachmentSize, AttachmentState, AttachmentTitle, AttachmentTrigger,
};
pub use avatar::{
    Avatar, AvatarBadge, AvatarFallback, AvatarGroup, AvatarGroupCount, AvatarImage, AvatarSize,
};
pub use badge::{Badge, BadgeSize, BadgeVariant, badge_class};
pub use bubble::{
    Bubble, BubbleAlign, BubbleContent, BubbleGroup, BubbleReactions, BubbleReactionsSide,
    BubbleVariant,
};
pub use button::{Button, ButtonSize, ButtonVariant, IconButton, button_class};
pub use button_group::{
    ButtonGroup, ButtonGroupOrientation, ButtonGroupSeparator, ButtonGroupText,
};
pub use card::{
    Card, CardAction, CardContent, CardDescription, CardFooter, CardHeader, CardItem, CardList,
    CardSize, CardTitle,
};
pub use checkbox::Checkbox;
pub use chip::{BindingChip, Chip};
pub use collapsible::{Collapsible, CollapsibleContent, CollapsibleTrigger};
pub use command::Command;
pub use command_input::CommandInput;
pub use command_item::CommandItem;
pub use command_layout::{
    CommandDescription, CommandEmpty, CommandFooter, CommandGroup, CommandGroupLabel,
    CommandHeader, CommandList, CommandTitle,
};
pub use context_menu::{
    ContextMenu, ContextMenuContent, ContextMenuGroup, ContextMenuItem, ContextMenuLabel,
    ContextMenuSeparator, ContextMenuTrigger,
};
pub use dialog::{
    Dialog, DialogAction, DialogBody, DialogClose, DialogContent, DialogDescription, DialogFooter,
    DialogHeader, DialogTitle, DialogTrigger,
};
pub use drawer::{
    Drawer, DrawerBody, DrawerClose, DrawerContent, DrawerDescription, DrawerFooter, DrawerHandle,
    DrawerHeader, DrawerPosition, DrawerTitle, DrawerTrigger, DrawerVariant,
};
pub use dropdown_menu::{
    DropdownMenu, DropdownMenuContent, DropdownMenuGroup, DropdownMenuItem, DropdownMenuLabel,
    DropdownMenuSeparator, DropdownMenuTrigger,
};
pub use dropdown_menu_sub::{
    DropdownMenuSub, DropdownMenuSubContent, DropdownMenuSubItem, DropdownMenuSubSide,
    DropdownMenuSubTrigger,
};
pub use empty::{
    Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyMedia, EmptyMediaVariant, EmptyState,
    EmptyTitle,
};
pub use field::{Field, FieldDescription, FieldError, FieldLabel};
pub use focus_scope::AutoFocusRequest;
pub use hover_card::{HoverCard, HoverCardContent, HoverCardSide, HoverCardTrigger};
pub use icon::Icon;
pub use icons::common::IconType;
pub use input::{Input, InputType};
pub use input_prompt::{
    InputPrompt, InputPromptFooter, InputPromptSubmit, InputPromptTextarea, InputPromptTools,
};
pub use item::{
    Item, ItemContent, ItemDescription, ItemMedia, ItemMediaVariant, ItemSize, ItemTitle,
    ItemVariant,
};
pub use kbd::{Kbd, KbdGroup};
pub use label::Label;
pub use list::{List, ListLayout, ListRow};
pub use markdown::Markdown;
pub use marker::{Marker, MarkerContent, MarkerIcon, MarkerVariant};
pub use message::{
    Message, MessageAlign, MessageAvatar, MessageContent, MessageFooter, MessageGroup,
    MessageHeader,
};
pub use metric_tile::MetricTile;
pub use popover::{Popover, PopoverContent, PopoverDescription, PopoverTitle, PopoverTrigger};
pub use progress::{Progress, ProgressTone};
pub use radio_button::{RadioGroup, RadioGroupItem};
pub use radio_button_group::{RadioButton, RadioButtonGroup, RadioButtonText};
pub use scroll_area::{
    ScrollArea, ScrollAreaCorner, ScrollAreaThumb, ScrollAreaViewport, ScrollBar,
    ScrollBarOrientation, SnapAreaVariant, SnapItem, SnapScrollArea, SnapVariant,
};
pub use section_title::SectionTitle;
pub use select::{Select, SelectOption};
pub use separator::{Separator, SeparatorOrientation};
pub use sheet::{
    Sheet, SheetBody, SheetClose, SheetContent, SheetDescription, SheetFooter, SheetHeader,
    SheetSide, SheetTitle, SheetTrigger,
};
pub use shimmer::Shimmer;
pub use sidenav::{Sidenav, SidenavTrigger, SidenavWrapper};
pub use sidenav_layout::{
    SidenavContent, SidenavFooter, SidenavGroup, SidenavGroupContent, SidenavGroupLabel,
    SidenavHeader, SidenavInner, SidenavInput, SidenavInset, SidenavMenu, SidenavMenuItem,
    SidenavMenuSub, SidenavMenuSubItem,
};
pub use sidenav_links::{
    DropdownMenuTriggerEllipsis, SidenavLink, SidenavMenuAction, SidenavMenuButton,
    SidenavMenuSubButton,
};
pub use sidenav_types::{
    SidenavCollapsible, SidenavContext, SidenavMenuButtonSize, SidenavMenuButtonVariant,
    SidenavSide, SidenavState, SidenavVariant,
};
pub use skeleton::Skeleton;
pub use slider::{Slider, SliderVariant};
pub use spinner::{Spinner, SpinnerCircle};
pub use status::{Status, StatusDot, StatusVariant};
pub use switch::{Switch, SwitchRow};
pub use table::{
    Table, TableBody, TableCaption, TableCell, TableFooter, TableHead, TableHeader, TableRow,
    TableWrapper,
};
pub use tabs::{Tabs, TabsContent, TabsList, TabsTrigger};
pub use text_field::TextField;
pub use textarea::Textarea;
pub use toast::{
    ToastContainer, ToastDirection, ToastList, ToastPosition, ToastToaster, ToastTrigger,
    ToastType, Toaster,
};
pub use toast_queue::{ToastHandle, use_toast};
pub use toggle_group::{
    ToggleGroup, ToggleGroupAction, ToggleGroupItem, ToggleGroupSpacing, ToggleGroupVariant,
};
pub use tooltip::{Tooltip, TooltipContent, TooltipPosition, TooltipProvider};
pub use usage_ring::UsageRing;
