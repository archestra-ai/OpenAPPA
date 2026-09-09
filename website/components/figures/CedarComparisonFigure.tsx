import styles from "./CedarComparisonFigure.module.css";

function Connector() {
  return <div className={styles.connector} aria-hidden="true">↓</div>;
}

export function CedarComparisonFigure() {
  return (
    <figure className={styles.figure} aria-label="Cedar and OpenAPPA: what you build and what is included">
      <div className={styles.toolbar}>
        <span><i className={styles.redKey} />Red: you implement</span>
        <span><i className={styles.greenKey} />Green: included</span>
      </div>
      <div className={styles.columns}>
        <section className={styles.column} aria-label="With Cedar">
          <h3>With Cedar</h3>
          <div className={styles.node}><strong>Agent</strong><span>Proposes a tool call</span></div>
          <Connector />
          <div className={`${styles.node} ${styles.policy}`}>
            <strong>Cedar policies</strong><span>Access rules in the Cedar language</span>
          </div>
          <Connector />
          <div className={styles.application}>
            <div className={styles.groupTitle}>Your application</div>
            <div className={`${styles.node} ${styles.custom}`}>
              <strong>Agent history</strong>
              <span>Keep track of what the agent reads and does.</span>
            </div>
            <Connector />
            <div className={`${styles.node} ${styles.builtin}`}>
              <strong>Cedar engine</strong>
              <span>Checks whether an action is allowed using the facts you provide.</span>
            </div>
            <Connector />
            <div className={`${styles.node} ${styles.custom}`}>
              <strong>Recovery</strong>
              <span>Build ways to remove private details or ask for approval.</span>
            </div>
          </div>
          <Connector />
          <div className={styles.node}><strong>Tool</strong><span>Your integration releases only allowed calls.</span></div>
        </section>
        <section className={styles.column} aria-label="With OpenAPPA">
          <h3>With OpenAPPA</h3>
          <div className={styles.node}><strong>Agent</strong><span>Proposes a tool call</span></div>
          <Connector />
          <div className={`${styles.node} ${styles.policy}`}>
            <strong>OpenAPPA policy</strong><span>Tool restrictions, requirements, and permitted remedies</span>
          </div>
          <Connector />
          <div className={`${styles.application} ${styles.platform}`}>
            <div className={styles.groupTitle}>OpenAPPA</div>
            <div className={`${styles.node} ${styles.builtin}`}>
              <strong>Agent history</strong>
              <span>Keeps track of what the agent reads and does.</span>
            </div>
            <Connector />
            <div className={`${styles.node} ${styles.builtin}`}>
              <strong>Policy engine</strong>
              <span>Checks whether an action is allowed based on what happened earlier.</span>
            </div>
            <Connector />
            <div className={`${styles.node} ${styles.builtin}`}>
              <strong>Recovery</strong>
              <span>Can arrange cleaning or approval when your rules allow it.</span>
            </div>
          </div>
          <Connector />
          <div className={styles.node}><strong>Tool</strong><span>Your integration releases only allowed calls.</span></div>
        </section>
      </div>
      <figcaption>
        Cedar checks the context you supply. OpenAPPA also carries data restrictions forward between actions.
      </figcaption>
    </figure>
  );
}
