pub mod kline;
pub mod plot;

use super::{Message, ViewState};
use self::plot::{AnySeries, Plot};
use iced::{
    Element,
    widget::{
        row,
    },
};
use std::{collections::BTreeMap, ops::RangeInclusive};

/// Creates the indicator plot and its labels. Wraps it under `iced::Element`(row).
pub fn indicator_row<'a, P, Y>(
    _main_chart: &'a ViewState,
    _plot: P,
    _datapoints: &'a BTreeMap<u64, Y>,
    _visible_range: RangeInclusive<u64>,
) -> Element<'a, Message>
where
    P: Plot<AnySeries<'a, Y>> + 'a,
{
    // TODO: Re-implement indicators with the new rendering pipeline
    row![].into()
}

