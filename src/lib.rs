use std::fmt;
use std::io;
use std::iter::once;

#[cfg(test)]
mod tests;

struct FormattedValue(f64);

impl fmt::Display for FormattedValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // If the value is not numeric, we need to represent it using Go library conventions.
        //
        // > `value` is a float represented as required by Go's ParseFloat() function.
        // > In addition to standard numerical values, NaN, +Inf, and -Inf are
        // > valid values representing not a number, positive infinity, and negative infinity, respectively.
        let value = self.0;
        if value.is_nan() {
            write!(f, "NaN")
        } else if value == f64::INFINITY {
            write!(f, "+Inf")
        } else if value == f64::NEG_INFINITY {
            write!(f, "-Inf")
        } else {
            write!(f, "{}", value)
        }
    }
}

/// A helper for encoding metrics that use
/// [labels](https://prometheus.io/docs/practices/naming/#labels).
/// See [MetricsEncoder::counter_vec] and [MetricsEncoder::gauge_vec].
pub struct LabeledMetricsBuilder<'a, W>
where
    W: io::Write,
{
    encoder: &'a mut MetricsEncoder<W>,
    name: &'a str,
}

impl<W: io::Write> LabeledMetricsBuilder<'_, W> {
    /// Encodes the metrics value observed for the specified values of labels.
    ///
    /// # Panics
    ///
    /// This function panics if one of the labels does not match pattern
    /// [a-zA-Z_][a-zA-Z0-9_]. See
    /// https://prometheus.io/docs/concepts/data_model/#metric-names-and-labels.
    pub fn value(self, labels: &[(&str, &str)], value: f64) -> io::Result<Self> {
        self.encoder
            .encode_value_with_labels(self.name, labels, value)?;
        Ok(self)
    }
}

/// A helper for encoding histograms that use
/// [labels](https://prometheus.io/docs/practices/naming/#labels).
/// See [MetricsEncoder::histogram_vec].
pub struct LabeledHistogramBuilder<'a, W>
where
    W: io::Write,
{
    encoder: &'a mut MetricsEncoder<W>,
    name: &'a str,
}

impl<W: io::Write> LabeledHistogramBuilder<'_, W> {
    /// Encodes the metrics histogram observed for the given values of labels.
    ///
    /// # Panics
    ///
    /// This function panics if one of the labels does not match pattern
    /// [a-zA-Z_][a-zA-Z0-9_]. See
    /// https://prometheus.io/docs/concepts/data_model/#metric-names-and-labels.
    pub fn histogram(
        self,
        labels: &[(&str, &str)],
        buckets: impl Iterator<Item = (f64, f64)>,
        sum: f64,
    ) -> io::Result<Self> {
        for (label, _) in labels.iter() {
            validate_prometheus_name(label);
        }

        let mut total: f64 = 0.0;
        let mut saw_infinity = false;
        for (bucket, v) in buckets {
            total += v;
            if bucket == std::f64::INFINITY {
                saw_infinity = true;
                writeln!(
                    self.encoder.writer,
                    "{}_bucket{{{}}} {} {}",
                    self.name,
                    MetricsEncoder::<W>::encode_labels(labels.iter().chain(once(&("le", "+Inf")))),
                    total,
                    self.encoder.now_millis
                )?;
            } else {
                let bucket_str = bucket.to_string();
                writeln!(
                    self.encoder.writer,
                    "{}_bucket{{{}}} {} {}",
                    self.name,
                    MetricsEncoder::<W>::encode_labels(
                        labels.iter().chain(once(&("le", bucket_str.as_str())))
                    ),
                    total,
                    self.encoder.now_millis
                )?;
            }
        }
        if !saw_infinity {
            writeln!(
                self.encoder.writer,
                "{}_bucket{{{}}} {} {}",
                self.name,
                MetricsEncoder::<W>::encode_labels(labels.iter().chain(once(&("le", "+Inf")))),
                total,
                self.encoder.now_millis
            )?;
        }

        if labels.is_empty() {
            writeln!(
                self.encoder.writer,
                "{}_sum {} {}",
                self.name,
                FormattedValue(sum),
                self.encoder.now_millis
            )?;
            writeln!(
                self.encoder.writer,
                "{}_count {} {}",
                self.name,
                FormattedValue(total),
                self.encoder.now_millis
            )?;
        } else {
            writeln!(
                self.encoder.writer,
                "{}_sum{{{}}} {} {}",
                self.name,
                MetricsEncoder::<W>::encode_labels(labels.iter()),
                FormattedValue(sum),
                self.encoder.now_millis
            )?;
            writeln!(
                self.encoder.writer,
                "{}_count{{{}}} {} {}",
                self.name,
                MetricsEncoder::<W>::encode_labels(labels.iter()),
                FormattedValue(total),
                self.encoder.now_millis
            )?;
        }

        Ok(self)
    }
}
/// `MetricsEncoder` provides methods to encode metrics in a text format
/// that can be understood by Prometheus.
///
/// Metrics are encoded with the block time included, to allow Prometheus
/// to discard out-of-order samples collected from replicas that are behind.
///
/// See [Exposition Formats][1] for an informal specification of the text
/// format.
///
/// [1]: https://github.com/prometheus/docs/blob/master/content/docs/instrumenting/exposition_formats.md
pub struct MetricsEncoder<W: io::Write> {
    writer: W,
    now_millis: i64,
}

impl<W: io::Write> MetricsEncoder<W> {
    /// Constructs a new encoder dumping metrics with the given timestamp into
    /// the specified writer.
    pub fn new(writer: W, now_millis: i64) -> Self {
        Self { writer, now_millis }
    }

    /// Returns the internal buffer that was used to record the
    /// metrics.
    pub fn into_inner(self) -> W {
        self.writer
    }

    fn encode_header(&mut self, name: &str, help: &str, typ: &str) -> io::Result<()> {
        writeln!(self.writer, "# HELP {} {}", name, help)?;
        writeln!(self.writer, "# TYPE {} {}", name, typ)
    }

    /// Encodes the metadata and the value of a histogram.
    ///
    /// SUM is the sum of all observed values, before they were put
    /// into buckets.
    ///
    /// BUCKETS is a list (key, value) pairs, where KEY is the bucket
    /// and VALUE is the number of items *in* this bucket (i.e., it's
    /// not a cumulative value).
    pub fn encode_histogram(
        &mut self,
        name: &str,
        buckets: impl Iterator<Item = (f64, f64)>,
        sum: f64,
        help: &str,
    ) -> io::Result<()> {
        self.histogram_vec(name, help)?
            .histogram(&[], buckets, sum)?;
        Ok(())
    }

    pub fn histogram_vec<'a>(
        &'a mut self,
        name: &'a str,
        help: &'a str,
    ) -> io::Result<LabeledHistogramBuilder<'a, W>> {
        validate_prometheus_name(name);
        self.encode_header(name, help, "histogram")?;
        Ok(LabeledHistogramBuilder {
            encoder: self,
            name,
        })
    }

    pub fn encode_single_value(
        &mut self,
        typ: &str,
        name: &str,
        value: f64,
        help: &str,
    ) -> io::Result<()> {
        validate_prometheus_name(name);
        self.encode_header(name, help, typ)?;
        writeln!(
            self.writer,
            "{} {} {}",
            name,
            FormattedValue(value),
            self.now_millis
        )
    }

    /// Encodes the metadata and the value of a counter.
    ///
    /// # Panics
    ///
    /// This function panics if the `name` argument does not match pattern [a-zA-Z_][a-zA-Z0-9_].
    pub fn encode_counter(&mut self, name: &str, value: f64, help: &str) -> io::Result<()> {
        self.encode_single_value("counter", name, value, help)
    }

    /// Encodes the metadata and the value of a gauge.
    ///
    /// # Panics
    ///
    /// This function panics if the `name` argument does not match pattern [a-zA-Z_][a-zA-Z0-9_].
    pub fn encode_gauge(&mut self, name: &str, value: f64, help: &str) -> io::Result<()> {
        self.encode_single_value("gauge", name, value, help)
    }

    /// Starts encoding of a counter that uses
    /// [labels](https://prometheus.io/docs/practices/naming/#labels).
    ///
    /// # Panics
    ///
    /// This function panics if the `name` argument does not match pattern [a-zA-Z_][a-zA-Z0-9_].
    pub fn counter_vec<'a>(
        &'a mut self,
        name: &'a str,
        help: &'a str,
    ) -> io::Result<LabeledMetricsBuilder<'a, W>> {
        validate_prometheus_name(name);
        self.encode_header(name, help, "counter")?;
        Ok(LabeledMetricsBuilder {
            encoder: self,
            name,
        })
    }

    /// Starts encoding of a gauge that uses
    /// [labels](https://prometheus.io/docs/practices/naming/#labels).
    ///
    /// # Panics
    ///
    /// This function panics if the `name` argument does not match pattern [a-zA-Z_][a-zA-Z0-9_].
    pub fn gauge_vec<'a>(
        &'a mut self,
        name: &'a str,
        help: &'a str,
    ) -> io::Result<LabeledMetricsBuilder<'a, W>> {
        validate_prometheus_name(name);
        self.encode_header(name, help, "gauge")?;
        Ok(LabeledMetricsBuilder {
            encoder: self,
            name,
        })
    }

    fn encode_labels<'a>(labels: impl Iterator<Item = &'a (&'a str, &'a str)>) -> String {
        let mut buf = String::new();
        for (i, (k, v)) in labels.enumerate() {
            validate_prometheus_name(k);
            if i > 0 {
                buf.push(',')
            }
            buf.push_str(k);
            buf.push('=');
            buf.push('"');
            for c in v.chars() {
                match c {
                    '\\' => {
                        buf.push('\\');
                        buf.push('\\');
                    }
                    '\n' => {
                        buf.push('\\');
                        buf.push('n');
                    }
                    '"' => {
                        buf.push('\\');
                        buf.push('"');
                    }
                    _ => buf.push(c),
                }
            }
            buf.push('"');
        }
        buf
    }

    fn encode_value_with_labels(
        &mut self,
        name: &str,
        label_values: &[(&str, &str)],
        value: f64,
    ) -> io::Result<()> {
        writeln!(
            self.writer,
            "{}{{{}}} {} {}",
            name,
            Self::encode_labels(label_values.iter()),
            FormattedValue(value),
            self.now_millis
        )
    }
}

/// Panics if the specified string is not a valid Prometheus metric/label name.
/// See https://prometheus.io/docs/concepts/data_model/#metric-names-and-labels.
fn validate_prometheus_name(name: &str) {
    if name.is_empty() {
        panic!("Empty names are not allowed");
    }
    let bytes = name.as_bytes();
    if (!bytes[0].is_ascii_alphabetic() && bytes[0] != b'_')
        || !bytes[1..]
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || *c == b'_')
    {
        panic!(
            "Name '{}' does not match pattern [a-zA-Z_][a-zA-Z0-9_]",
            name
        );
    }
}
