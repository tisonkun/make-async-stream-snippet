use futures::StreamExt;

#[tokio::main]

async fn main() {
    let sum = make_async_stream::make_stream(async move |tx| {
        for i in 1..=100 {
            let fut = async {
                tx.send(i).await;
            };
            fut.await;
        }
    })
    .fold(0, |acc, x| async move { acc + x })
    .await;

    println!("sum: {}", sum);
}
